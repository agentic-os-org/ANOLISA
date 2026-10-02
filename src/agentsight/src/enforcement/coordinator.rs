//! Desired-state coordinator between AgentSight, SQLite, and the enforcer.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    CredentialPolicySnapshot, HealthStatus, ViolationEvent,
};
use thiserror::Error;
use uuid::Uuid;

use super::store::credential_binding_matches_request;
use super::{EnforcementClient, EnforcementError, EnforcementStore, EnforcementStoreError};

mod reconciliation;
mod transition;

const INGESTION_UNAVAILABLE_MESSAGE: &str = "violation ingestion is not subscribed";
const DEGRADED_BINDING_MESSAGE: &str = "enforcement binding is degraded";
const FAILED_BINDING_MESSAGE: &str = "enforcement binding failed";
const CREDENTIAL_APPLY_CLIENT_CALLS: u32 = 3;
const CREDENTIAL_APPLY_MARGIN_CALLS: u32 = 1;

type WorkerTask = Box<dyn FnOnce() + Send + 'static>;

struct WorkerToken {
    _identity: (),
}

#[derive(Default)]
struct IngestionState {
    current: Option<Arc<WorkerToken>>,
    subscription_id: Option<Uuid>,
    ready: bool,
    message: Option<String>,
    generation: u64,
}

#[derive(Default)]
struct AvailabilityLogTransitions {
    unavailable: bool,
}

impl AvailabilityLogTransitions {
    fn unavailable(&mut self, detail: &str) -> Option<String> {
        if self.unavailable {
            return None;
        }
        self.unavailable = true;
        Some(format!("AgentSight enforcement unavailable: {detail}"))
    }

    fn recovered(&mut self) -> Option<String> {
        if std::mem::take(&mut self.unavailable) {
            Some("AgentSight enforcement recovered".into())
        } else {
            None
        }
    }
}

#[derive(Clone)]
struct IngestionReadiness {
    state: Arc<Mutex<IngestionState>>,
}

/// Opaque snapshot of one acknowledged required-subscription generation.
#[derive(Clone)]
pub(crate) struct IngestionLease {
    worker: Arc<WorkerToken>,
    subscription_id: Uuid,
    generation: u64,
}

#[derive(Clone)]
struct ReconciliationLease {
    worker: Arc<WorkerToken>,
    subscription_id: Uuid,
    generation: u64,
}

/// Guard that prevents the leased ingestion generation from changing.
pub(crate) struct IngestionGenerationLease<'a> {
    _state: MutexGuard<'a, IngestionState>,
}

impl IngestionReadiness {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(IngestionState::default())),
        }
    }

    fn candidate(&self) -> Arc<WorkerToken> {
        Arc::new(WorkerToken { _identity: () })
    }

    fn install(&self, worker: Arc<WorkerToken>) {
        let mut state = self.state();
        state.current = Some(worker);
        state.subscription_id = None;
        state.ready = false;
        state.message = Some(INGESTION_UNAVAILABLE_MESSAGE.into());
        advance_generation(&mut state);
    }

    fn stop(&self) {
        let mut state = self.state();
        state.current = None;
        state.subscription_id = None;
        state.ready = false;
        state.message = Some(INGESTION_UNAVAILABLE_MESSAGE.into());
        advance_generation(&mut state);
    }

    fn clear_if_current(&self, worker: &Arc<WorkerToken>) {
        let mut state = self.state();
        if state
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            state.current = None;
            state.subscription_id = None;
            state.ready = false;
            state.message = Some(INGESTION_UNAVAILABLE_MESSAGE.into());
            advance_generation(&mut state);
        }
    }

    fn adopt_subscription(&self, worker: &Arc<WorkerToken>, subscription_id: Uuid) -> bool {
        let mut state = self.state();
        if state
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            if state.subscription_id != Some(subscription_id) {
                state.subscription_id = Some(subscription_id);
                advance_generation(&mut state);
            }
            true
        } else {
            false
        }
    }

    fn mark_ready(&self, worker: &Arc<WorkerToken>, subscription_id: Uuid) -> bool {
        let mut state = self.state();
        if state.subscription_id == Some(subscription_id)
            && state
                .current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            if !state.ready || state.message.is_some() {
                state.ready = true;
                state.message = None;
                advance_generation(&mut state);
            }
            true
        } else {
            false
        }
    }

    fn mark_not_ready(&self, worker: &Arc<WorkerToken>) {
        let mut state = self.state();
        if state
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            if state.ready
                || state.subscription_id.is_some()
                || state.message.as_deref() != Some(INGESTION_UNAVAILABLE_MESSAGE)
            {
                state.ready = false;
                state.subscription_id = None;
                state.message = Some(INGESTION_UNAVAILABLE_MESSAGE.into());
                advance_generation(&mut state);
            }
        }
    }

    fn mark_unavailable(&self, worker: &Arc<WorkerToken>, message: String) {
        let mut state = self.state();
        if state
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            if state.ready || state.message.as_deref() != Some(message.as_str()) {
                state.ready = false;
                state.message = Some(message);
                advance_generation(&mut state);
            }
        }
    }

    fn lease(&self) -> Option<IngestionLease> {
        let state = self.state();
        if !state.ready {
            return None;
        }
        match (state.current.as_ref(), state.subscription_id) {
            (Some(worker), Some(subscription_id)) => Some(IngestionLease {
                worker: Arc::clone(worker),
                subscription_id,
                generation: state.generation,
            }),
            _ => None,
        }
    }

    fn commit_if_current<E>(
        &self,
        lease: &IngestionLease,
        commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let state = self.state();
        if !lease.matches(&state) {
            return Ok(false);
        }
        commit()?;
        Ok(true)
    }

    fn reconciliation_lease(
        &self,
        worker: &Arc<WorkerToken>,
        subscription_id: Uuid,
    ) -> Option<ReconciliationLease> {
        let state = self.state();
        (state.subscription_id == Some(subscription_id)
            && state
                .current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, worker)))
        .then(|| ReconciliationLease {
            worker: Arc::clone(worker),
            subscription_id,
            generation: state.generation,
        })
    }

    fn commit_reconciliation_if_current<E>(
        &self,
        lease: &ReconciliationLease,
        commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let state = self.state();
        if state.generation != lease.generation
            || state.subscription_id != Some(lease.subscription_id)
            || !state
                .current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &lease.worker))
        {
            return Ok(false);
        }
        commit()?;
        Ok(true)
    }

    fn lease_current(&self, lease: &IngestionLease) -> Option<IngestionGenerationLease<'_>> {
        let state = self.state();
        lease
            .matches(&state)
            .then_some(IngestionGenerationLease { _state: state })
    }

    fn invalidate_lease(&self, lease: &IngestionLease, message: String) {
        let mut state = self.state();
        if state.subscription_id == Some(lease.subscription_id)
            && state
                .current
                .as_ref()
                .is_some_and(|worker| Arc::ptr_eq(worker, &lease.worker))
        {
            state.ready = false;
            state.subscription_id = None;
            state.message = Some(message);
            advance_generation(&mut state);
        }
    }

    #[cfg(test)]
    fn is_ready(&self) -> bool {
        self.state().ready
    }

    fn status(&self) -> (bool, Option<String>) {
        let state = self.state();
        (state.ready, state.message.clone())
    }

    fn is_current(&self, worker: &Arc<WorkerToken>) -> bool {
        self.state()
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, worker))
    }

    fn is_subscription_current(&self, worker: &Arc<WorkerToken>, subscription_id: Uuid) -> bool {
        let state = self.state();
        state.subscription_id == Some(subscription_id)
            && state
                .current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, worker))
    }

    fn state(&self) -> MutexGuard<'_, IngestionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl IngestionLease {
    fn is_current(&self, readiness: &IngestionReadiness) -> bool {
        let state = readiness.state();
        self.matches(&state)
    }

    fn matches(&self, state: &IngestionState) -> bool {
        state.ready
            && state.generation == self.generation
            && state.subscription_id == Some(self.subscription_id)
            && state
                .current
                .as_ref()
                .is_some_and(|worker| Arc::ptr_eq(worker, &self.worker))
    }
}

fn advance_generation(state: &mut IngestionState) {
    state.generation = state.generation.wrapping_add(1);
}

/// Coordination failures across the UDS and persistence boundaries.
#[derive(Debug, Error)]
pub enum EnforcementCoordinatorError {
    /// A violation subscriber has not completed its acknowledgement handshake.
    #[error("{INGESTION_UNAVAILABLE_MESSAGE}")]
    IngestionUnavailable,
    /// Combined backend or ingestion health cannot guarantee violation evidence.
    #[error("enforcement unavailable: {0}")]
    EnforcementUnavailable(String),
    /// Runtime policy ownership could not be proved and requires reconciliation.
    #[error("policy replacement ownership is indeterminate; reconciliation is required")]
    TransitionUnavailable,
    /// The privileged service call failed.
    #[error(transparent)]
    Client(#[from] EnforcementError),
    /// Desired state or evidence persistence failed.
    #[error(transparent)]
    Store(#[from] EnforcementStoreError),
    /// Desired state was persisted, but the enforcer did not acknowledge it.
    ///
    /// Distinguishes persisted-but-unacknowledged applies from failures that
    /// happened before persistence, so callers can report store truth.
    #[error("binding {binding_id} persisted as {state:?}: {source}")]
    PersistedUnacknowledged {
        /// Persisted binding identifier.
        binding_id: Uuid,
        /// Terminal persisted state at failure time.
        state: BindingState,
        /// Underlying apply failure.
        #[source]
        source: Box<EnforcementCoordinatorError>,
    },
    /// The ingestion worker could not be created.
    #[error("start enforcement ingestion: {0}")]
    Thread(#[from] std::io::Error),
}

/// AgentSight owner of desired policy state and violation ingestion.
pub struct EnforcementCoordinator {
    client: EnforcementClient,
    store: EnforcementStore,
    ingestion_readiness: IngestionReadiness,
    lifecycle: Arc<Mutex<()>>,
}

impl EnforcementCoordinator {
    /// Creates a coordinator without starting background ingestion.
    pub fn new(client: EnforcementClient, store: EnforcementStore) -> Self {
        Self {
            client,
            store,
            ingestion_readiness: IngestionReadiness::new(),
            lifecycle: Arc::new(Mutex::new(())),
        }
    }

    /// Bounds foreground ownership across health, apply, post-apply health, and scheduling slack.
    pub(crate) fn credential_apply_claim_lease(&self) -> Duration {
        self.client.request_timeout().saturating_mul(
            CREDENTIAL_APPLY_CLIENT_CALLS.saturating_add(CREDENTIAL_APPLY_MARGIN_CALLS),
        )
    }

    /// Persists pending desired state, then applies and persists acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a persistence error or the enforcer rejection after recording a
    /// sanitized failed state. Returns an availability error before persisting
    /// when combined backend and violation-ingestion health is not ready. Once
    /// the pending state is persisted, later failures return
    /// [`EnforcementCoordinatorError::PersistedUnacknowledged`] so callers can
    /// see that the binding remains in the store.
    pub fn apply(&self, request: ApplyPolicy) -> Result<Binding, EnforcementCoordinatorError> {
        let _lifecycle = self.lifecycle();
        if let Some(existing) = self.store.binding(request.binding_id)?
            && existing.request == request
            && matches!(
                existing.state,
                BindingState::Detaching | BindingState::Detached
            )
        {
            return Ok(existing);
        }
        let lease = self
            .ingestion_readiness
            .lease()
            .ok_or(EnforcementCoordinatorError::IngestionUnavailable)?;
        let health = combine_health(self.client.health()?, &self.ingestion_readiness);
        if !health.ready {
            return Err(unavailable_from_health(health));
        }
        let pending = Binding {
            request: request.clone(),
            state: BindingState::Pending,
            message: None,
            domain_id: None,
        };
        if let Err(error) = self.store.upsert_binding(&pending) {
            return match error {
                EnforcementStoreError::BindingConflict(binding_id) => {
                    Err(EnforcementError::Remote {
                        code: "binding_conflict".into(),
                        message: format!(
                            "binding {binding_id} conflicts with persisted desired state"
                        ),
                    }
                    .into())
                }
                error => Err(error.into()),
            };
        }
        match self.client.apply(request.clone(), lease.subscription_id) {
            Ok(binding) => {
                let post_apply_health = self
                    .client
                    .health()
                    .map(|health| combine_health(health, &self.ingestion_readiness));
                match post_apply_health {
                    Ok(health) if health.ready => {
                        if self
                            .ingestion_readiness
                            .commit_if_current(&lease, || self.store.upsert_binding(&binding))?
                        {
                            Ok(binding)
                        } else {
                            let message =
                                "violation ingestion readiness changed during policy apply";
                            let binding_id = binding.request.binding_id;
                            self.persist_degraded_binding(binding, message)?;
                            Err(persisted_unacknowledged(
                                binding_id,
                                BindingState::Degraded,
                                EnforcementCoordinatorError::EnforcementUnavailable(message.into()),
                            ))
                        }
                    }
                    Ok(health) => {
                        let detail = health_message(&health);
                        log::error!("enforcement became unavailable after policy apply: {detail}");
                        let binding_id = binding.request.binding_id;
                        self.persist_degraded_binding(binding, &detail)?;
                        Err(persisted_unacknowledged(
                            binding_id,
                            BindingState::Degraded,
                            EnforcementCoordinatorError::EnforcementUnavailable(detail),
                        ))
                    }
                    Err(error) => {
                        log::error!("enforcement health check failed after policy apply: {error}");
                        let binding_id = binding.request.binding_id;
                        self.persist_degraded_binding(binding, DEGRADED_BINDING_MESSAGE)?;
                        Err(persisted_unacknowledged(
                            binding_id,
                            BindingState::Degraded,
                            error.into(),
                        ))
                    }
                }
            }
            Err(EnforcementError::Remote { code, message })
                if code == "required_subscription_unavailable" =>
            {
                log::error!("required violation subscription unavailable: {message}");
                self.ingestion_readiness
                    .invalidate_lease(&lease, INGESTION_UNAVAILABLE_MESSAGE.into());
                let binding_id = request.binding_id;
                self.store.upsert_binding(&Binding {
                    request,
                    state: BindingState::Degraded,
                    message: Some(DEGRADED_BINDING_MESSAGE.into()),
                    domain_id: None,
                })?;
                Err(persisted_unacknowledged(
                    binding_id,
                    BindingState::Degraded,
                    EnforcementCoordinatorError::EnforcementUnavailable(
                        INGESTION_UNAVAILABLE_MESSAGE.into(),
                    ),
                ))
            }
            Err(error) => {
                log::error!("enforcement policy apply failed: {error}");
                let binding_id = request.binding_id;
                self.store.upsert_binding(&Binding {
                    request,
                    state: BindingState::Failed,
                    message: Some(FAILED_BINDING_MESSAGE.into()),
                    domain_id: None,
                })?;
                Err(persisted_unacknowledged(
                    binding_id,
                    BindingState::Failed,
                    error.into(),
                ))
            }
        }
    }

    /// Applies a product-level credential policy and persists the adapter acknowledgement.
    ///
    /// Product policy compilation remains inside the privileged adapter. AgentSight persists
    /// the structured request first, then atomically records its compiled acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns when ingestion is unavailable, the adapter rejects the policy, or persistence
    /// fails.
    pub fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
    ) -> Result<Binding, EnforcementCoordinatorError> {
        let _lifecycle = self.lifecycle();
        let lease = self
            .ingestion_readiness
            .lease()
            .ok_or(EnforcementCoordinatorError::IngestionUnavailable)?;
        let health = combine_health(self.client.health()?, &self.ingestion_readiness);
        if !health.ready {
            return Err(unavailable_from_health(health));
        }
        let intent = match self.store.begin_credential_policy_intent(&request) {
            Ok(intent) => intent,
            Err(EnforcementStoreError::CredentialIntentConflict(binding_id)) => {
                return Err(EnforcementError::Remote {
                    code: "binding_conflict".into(),
                    message: format!(
                        "binding {binding_id} conflicts with persisted credential intent"
                    ),
                }
                .into());
            }
            Err(error) => return Err(error.into()),
        };
        if matches!(
            intent.state,
            BindingState::Detaching | BindingState::Detached
        ) {
            return self
                .store
                .binding(request.binding_id)?
                .ok_or(EnforcementStoreError::MissingBinding(request.binding_id).into());
        }
        let policy = request.policy.clone();
        match self
            .client
            .apply_credential_policy(request.clone(), lease.subscription_id)
        {
            Ok(binding) => {
                if !credential_binding_matches_request(&request, &binding) {
                    return Err(EnforcementStoreError::InvalidCredentialPolicySnapshot {
                        binding_id: request.binding_id,
                        reason: "compiled acknowledgement does not match structured intent".into(),
                    }
                    .into());
                }
                let post_apply_health = self
                    .client
                    .health()
                    .map(|health| combine_health(health, &self.ingestion_readiness));
                match post_apply_health {
                    Ok(health) if health.ready => {
                        if self.ingestion_readiness.commit_if_current(&lease, || {
                            self.store.upsert_credential_binding(&binding, &policy)
                        })? {
                            Ok(binding)
                        } else {
                            let message =
                                "violation ingestion readiness changed during credential apply";
                            self.persist_degraded_credential_binding(binding, message, &policy)?;
                            Err(EnforcementCoordinatorError::EnforcementUnavailable(
                                message.into(),
                            ))
                        }
                    }
                    Ok(health) => {
                        let detail = health_message(&health);
                        log::error!(
                            "enforcement became unavailable after credential apply: {detail}"
                        );
                        self.persist_degraded_credential_binding(
                            binding,
                            DEGRADED_BINDING_MESSAGE,
                            &policy,
                        )?;
                        Err(EnforcementCoordinatorError::EnforcementUnavailable(
                            "enforcement service became unavailable after credential apply".into(),
                        ))
                    }
                    Err(error) => {
                        log::error!(
                            "enforcement health check failed after credential apply: {error}"
                        );
                        self.persist_degraded_credential_binding(
                            binding,
                            DEGRADED_BINDING_MESSAGE,
                            &policy,
                        )?;
                        Err(error.into())
                    }
                }
            }
            Err(EnforcementError::Remote { code, message })
                if code == "required_subscription_unavailable" =>
            {
                log::error!("required violation subscription unavailable: {message}");
                self.ingestion_readiness
                    .invalidate_lease(&lease, INGESTION_UNAVAILABLE_MESSAGE.into());
                Err(EnforcementCoordinatorError::EnforcementUnavailable(
                    INGESTION_UNAVAILABLE_MESSAGE.into(),
                ))
            }
            Err(EnforcementError::Remote { code, message })
                if reconciliation::is_binding_rejection(&code) =>
            {
                self.store.mark_credential_policy_intent_failed(
                    request.binding_id,
                    &reconciliation::remote_rejection_message(&code, &message),
                )?;
                Err(EnforcementError::Remote { code, message }.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Persists detaching state and waits for acknowledgement before detached.
    ///
    /// # Errors
    ///
    /// Returns a missing-binding, persistence, or enforcer error.
    pub fn detach(&self, binding_id: Uuid) -> Result<(), EnforcementCoordinatorError> {
        let _lifecycle = self.lifecycle();
        let mut binding = self
            .store
            .binding(binding_id)?
            .ok_or(EnforcementStoreError::MissingBinding(binding_id))?;
        if binding.state == BindingState::Detached {
            return Ok(());
        }
        binding.state = BindingState::Detaching;
        self.store.upsert_binding(&binding)?;
        match self.client.detach(binding_id) {
            Ok(()) => {
                binding.state = BindingState::Detached;
                binding.message = None;
                binding.domain_id = None;
                self.store.upsert_binding(&binding)?;
                Ok(())
            }
            Err(EnforcementError::Remote { code, .. }) if code == "missing_binding" => {
                binding.state = BindingState::Detached;
                binding.message = None;
                binding.domain_id = None;
                self.store.upsert_binding(&binding)?;
                Ok(())
            }
            Err(error) => {
                log::error!("enforcement detach failed: {error}");
                binding.message = Some(FAILED_BINDING_MESSAGE.into());
                self.store.upsert_binding(&binding)?;
                Err(error.into())
            }
        }
    }

    /// Lists persisted binding state.
    ///
    /// Returns every persisted binding, including detached terminal state;
    /// containment and reconciliation rely on the full snapshot.
    ///
    /// # Errors
    ///
    /// Returns a persistence error.
    pub fn bindings(&self) -> Result<Vec<Binding>, EnforcementCoordinatorError> {
        Ok(self.store.bindings()?)
    }

    /// Lists persisted bindings for API consumers, hiding detached bindings.
    ///
    /// Detached bindings acknowledge successful deletion and would otherwise
    /// resurface as zombie rows in the bindings list. Failed and degraded
    /// bindings stay visible so operators can still inspect them.
    ///
    /// # Errors
    ///
    /// Returns a persistence error.
    pub fn active_bindings(&self) -> Result<Vec<Binding>, EnforcementCoordinatorError> {
        Ok(self
            .store
            .bindings()?
            .into_iter()
            .filter(|binding| binding.state != BindingState::Detached)
            .collect())
    }

    /// Reads immutable structured provenance for one credential binding.
    pub(crate) fn credential_policy_snapshot(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, EnforcementCoordinatorError> {
        Ok(self.store.credential_policy_snapshot(binding_id)?)
    }

    /// Snapshots the current acknowledged required-subscription generation.
    pub(crate) fn ingestion_generation(
        &self,
    ) -> Result<IngestionLease, EnforcementCoordinatorError> {
        self.ingestion_readiness
            .lease()
            .ok_or(EnforcementCoordinatorError::IngestionUnavailable)
    }

    /// Returns whether an earlier generation snapshot remains current.
    pub(crate) fn ingestion_generation_is_current(&self, lease: &IngestionLease) -> bool {
        lease.is_current(&self.ingestion_readiness)
    }

    /// Holds the readiness lock only when the expected generation is still current.
    pub(crate) fn lease_ingestion_generation(
        &self,
        lease: &IngestionLease,
    ) -> Option<IngestionGenerationLease<'_>> {
        self.ingestion_readiness.lease_current(lease)
    }

    /// Lists newest persisted violations.
    ///
    /// # Errors
    ///
    /// Returns a persistence error.
    pub fn violations(
        &self,
        limit: usize,
    ) -> Result<Vec<ViolationEvent>, EnforcementCoordinatorError> {
        Ok(self.store.violations(limit)?)
    }

    /// Starts bounded reconnecting violation ingestion.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the worker thread cannot be spawned or activated.
    pub fn start_ingestion(&self) -> Result<JoinHandle<()>, EnforcementCoordinatorError> {
        self.start_ingestion_with(|worker| {
            thread::Builder::new()
                .name("agentsight-enforcement-ingestion".into())
                .spawn(worker)
        })
    }

    fn start_ingestion_with<F>(
        &self,
        spawn: F,
    ) -> Result<JoinHandle<()>, EnforcementCoordinatorError>
    where
        F: FnOnce(WorkerTask) -> Result<JoinHandle<()>, std::io::Error>,
    {
        let worker = self.ingestion_readiness.candidate();
        let client = self.client.clone();
        let store = self.store.clone();
        let ingestion_readiness = self.ingestion_readiness.clone();
        let lifecycle = Arc::clone(&self.lifecycle);
        let worker_token = Arc::clone(&worker);
        let (activate, activation) = mpsc::sync_channel(0);
        let task = Box::new(move || {
            if activation.recv().is_ok() {
                ingest_loop(client, store, ingestion_readiness, lifecycle, worker_token);
            }
        });
        let handle = spawn(task)?;
        self.ingestion_readiness.install(Arc::clone(&worker));
        if activate.send(()).is_err() {
            self.ingestion_readiness.clear_if_current(&worker);
            return Err(EnforcementCoordinatorError::Thread(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "activate enforcement ingestion worker",
            )));
        }
        Ok(handle)
    }

    /// Requests the ingestion worker to stop at its next bounded read interval.
    pub fn stop_ingestion(&self) {
        self.ingestion_readiness.stop();
    }

    /// Queries backend readiness and requires an acknowledged violation subscriber.
    ///
    /// # Errors
    ///
    /// Returns a client error when the enforcer cannot be reached.
    pub fn health(
        &self,
    ) -> Result<agentsight_enforcement_protocol::HealthStatus, EnforcementCoordinatorError> {
        let mut health = combine_health(self.client.health()?, &self.ingestion_readiness);
        if self
            .store
            .pending_transitions()?
            .iter()
            .any(|transition| transition.phase == super::TransitionPhase::Indeterminate)
        {
            health.ready = false;
            health.message = Some(
                "policy replacement ownership is indeterminate; reconciliation is required".into(),
            );
        }
        Ok(health)
    }

    fn lifecycle(&self) -> MutexGuard<'_, ()> {
        self.lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn persist_degraded_binding(
        &self,
        mut binding: Binding,
        message: &str,
    ) -> Result<(), EnforcementStoreError> {
        binding.state = BindingState::Degraded;
        binding.message = Some(message.into());
        self.store.upsert_binding(&binding)
    }

    fn persist_degraded_credential_binding(
        &self,
        mut binding: Binding,
        message: &str,
        policy: &CredentialExfiltrationPolicy,
    ) -> Result<(), EnforcementStoreError> {
        binding.state = BindingState::Degraded;
        binding.message = Some(message.into());
        self.store.upsert_credential_binding(&binding, policy)
    }
}

fn unavailable_from_health(health: HealthStatus) -> EnforcementCoordinatorError {
    EnforcementCoordinatorError::EnforcementUnavailable(health_message(&health))
}

fn persisted_unacknowledged(
    binding_id: Uuid,
    state: BindingState,
    source: EnforcementCoordinatorError,
) -> EnforcementCoordinatorError {
    EnforcementCoordinatorError::PersistedUnacknowledged {
        binding_id,
        state,
        source: Box::new(source),
    }
}

fn health_message(health: &HealthStatus) -> String {
    health
        .message
        .clone()
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| format!("{} backend is not ready", health.backend))
}

fn combine_health(
    mut health: HealthStatus,
    ingestion_readiness: &IngestionReadiness,
) -> HealthStatus {
    let (ingestion_ready, ingestion_message) = ingestion_readiness.status();
    health.ready &= ingestion_ready;
    if ingestion_ready {
        return health;
    }

    let ingestion_message =
        ingestion_message.unwrap_or_else(|| INGESTION_UNAVAILABLE_MESSAGE.into());
    health.message = Some(match health.message.take() {
        Some(backend_message)
            if !backend_message.is_empty() && backend_message != ingestion_message =>
        {
            format!("{backend_message}; {ingestion_message}")
        }
        Some(backend_message) if !backend_message.is_empty() => backend_message,
        _ => ingestion_message,
    });
    health
}

fn ingest_loop(
    client: EnforcementClient,
    store: EnforcementStore,
    ingestion_readiness: IngestionReadiness,
    lifecycle: Arc<Mutex<()>>,
    worker: Arc<WorkerToken>,
) {
    let mut backoff = Duration::from_millis(100);
    let mut availability_log = AvailabilityLogTransitions::default();
    while ingestion_readiness.is_current(&worker) {
        ingestion_readiness.mark_not_ready(&worker);
        match client.subscribe_required() {
            Ok(mut subscription) => {
                let subscription_id = subscription.subscription_id();
                if !ingestion_readiness.is_current(&worker) {
                    break;
                }
                let reconciliation = {
                    let _lifecycle = lifecycle
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !ingestion_readiness.adopt_subscription(&worker, subscription_id) {
                        break;
                    }
                    let Some(reconciliation_lease) =
                        ingestion_readiness.reconciliation_lease(&worker, subscription_id)
                    else {
                        break;
                    };
                    match reconciliation::reconcile_desired_state_fenced(
                        &client,
                        &store,
                        subscription_id,
                        |store, key, outcome| {
                            ingestion_readiness
                                .commit_reconciliation_if_current(&reconciliation_lease, || {
                                    transition::persist_outcome(store, key, outcome)
                                })
                        },
                    ) {
                        Ok(()) if ingestion_readiness.mark_ready(&worker, subscription_id) => {
                            Ok(())
                        }
                        Ok(()) => break,
                        Err(error) => Err(error),
                    }
                };
                if let Err(error) = reconciliation {
                    if ingestion_readiness.is_current(&worker) {
                        let detail = format!("reconciliation failed: {error}");
                        if let Some(message) = availability_log.unavailable(&detail) {
                            eprintln!("{message}");
                        }
                        if let Err(store_error) =
                            store.mark_active_degraded(DEGRADED_BINDING_MESSAGE)
                        {
                            eprintln!(
                                "AgentSight could not persist enforcement degradation: {store_error}"
                            );
                        }
                    }
                    sleep_until_superseded(&ingestion_readiness, &worker, backoff);
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(5));
                    continue;
                }
                if let Some(message) = availability_log.recovered() {
                    eprintln!("{message}");
                }
                backoff = Duration::from_millis(100);
                while ingestion_readiness.is_subscription_current(&worker, subscription_id) {
                    match subscription.next_event() {
                        Ok(Some(event)) => {
                            if !ingestion_readiness
                                .is_subscription_current(&worker, subscription_id)
                            {
                                break;
                            }
                            if !persist_violation_until_stored(
                                &store,
                                &ingestion_readiness,
                                &worker,
                                subscription_id,
                                &event,
                            ) {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            ingestion_readiness.mark_not_ready(&worker);
                            if !ingestion_readiness.is_current(&worker) {
                                break;
                            }
                            let detail = format!("subscription lost: {error}");
                            if let Some(message) = availability_log.unavailable(&detail) {
                                eprintln!("{message}");
                            }
                            if let Err(store_error) =
                                store.mark_active_degraded(DEGRADED_BINDING_MESSAGE)
                            {
                                eprintln!(
                                    "AgentSight could not persist enforcement degradation: {store_error}"
                                );
                            }
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                ingestion_readiness.mark_not_ready(&worker);
                if ingestion_readiness.is_current(&worker) {
                    if let Some(message) = availability_log.unavailable(&error.to_string()) {
                        eprintln!("{message}");
                    }
                    if let Err(store_error) = store.mark_active_degraded(DEGRADED_BINDING_MESSAGE) {
                        eprintln!(
                            "AgentSight could not persist enforcement unavailability: {store_error}"
                        );
                    }
                }
            }
        }
        ingestion_readiness.mark_not_ready(&worker);
        sleep_until_superseded(&ingestion_readiness, &worker, backoff);
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(5));
    }
    ingestion_readiness.mark_not_ready(&worker);
}

fn persist_violation_until_stored(
    store: &EnforcementStore,
    ingestion_readiness: &IngestionReadiness,
    worker: &Arc<WorkerToken>,
    subscription_id: Uuid,
    event: &ViolationEvent,
) -> bool {
    let mut backoff = Duration::from_millis(100);
    loop {
        if !ingestion_readiness.is_current(worker) {
            return false;
        }
        match store.insert_violation(event) {
            Ok(_) => return ingestion_readiness.mark_ready(worker, subscription_id),
            Err(error) => {
                ingestion_readiness.mark_unavailable(worker, INGESTION_UNAVAILABLE_MESSAGE.into());
                eprintln!("AgentSight could not persist enforcement event: {error}");
                sleep_until_superseded(ingestion_readiness, worker, backoff);
                backoff = backoff.saturating_mul(2).min(Duration::from_secs(5));
            }
        }
    }
}

fn sleep_until_superseded(
    ingestion_readiness: &IngestionReadiness,
    worker: &Arc<WorkerToken>,
    duration: Duration,
) {
    let step = Duration::from_millis(50);
    let mut elapsed = Duration::ZERO;
    while elapsed < duration && ingestion_readiness.is_current(worker) {
        let remaining = duration.saturating_sub(elapsed);
        let sleep = remaining.min(step);
        thread::sleep(sleep);
        elapsed += sleep;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use agentsight_enforcement_protocol::{
        PROTOCOL_VERSION, RemoteError, Request, Response, ResponseBody, read_frame, write_frame,
    };

    use super::*;

    #[test]
    fn availability_log_reports_only_state_transitions() {
        let mut transitions = AvailabilityLogTransitions::default();

        assert_eq!(
            transitions.unavailable("socket missing"),
            Some("AgentSight enforcement unavailable: socket missing".into())
        );
        assert_eq!(transitions.unavailable("socket missing"), None);
        assert_eq!(transitions.unavailable("connection refused"), None);
        assert_eq!(
            transitions.recovered(),
            Some("AgentSight enforcement recovered".into())
        );
        assert_eq!(transitions.recovered(), None);
        assert_eq!(
            transitions.unavailable("socket missing"),
            Some("AgentSight enforcement unavailable: socket missing".into())
        );
    }

    fn make_ready(readiness: &IngestionReadiness, worker: &Arc<WorkerToken>) -> Uuid {
        let subscription_id = Uuid::new_v4();
        assert!(readiness.adopt_subscription(worker, subscription_id));
        assert!(readiness.mark_ready(worker, subscription_id));
        subscription_id
    }

    #[test]
    fn failed_start_does_not_supersede_ready_worker() {
        let coordinator = EnforcementCoordinator::new(
            EnforcementClient::new("/tmp/unused-enforcement.sock"),
            EnforcementStore::open(":memory:").expect("test store should open"),
        );
        let active = coordinator.ingestion_readiness.candidate();
        coordinator.ingestion_readiness.install(Arc::clone(&active));
        make_ready(&coordinator.ingestion_readiness, &active);

        let result = coordinator
            .start_ingestion_with(|_| Err(std::io::Error::other("fixture thread spawn failure")));

        assert!(matches!(
            result,
            Err(EnforcementCoordinatorError::Thread(_))
        ));
        assert!(coordinator.ingestion_readiness.is_current(&active));
        assert!(coordinator.ingestion_readiness.is_ready());
    }

    #[test]
    fn credential_apply_claim_lease_scales_with_configured_request_timeout() {
        let coordinator = EnforcementCoordinator::new(
            EnforcementClient::new("/tmp/unused-enforcement.sock")
                .with_timeout(Duration::from_secs(20)),
            EnforcementStore::open(":memory:").expect("test store should open"),
        );

        assert_eq!(
            coordinator.credential_apply_claim_lease(),
            Duration::from_secs(80)
        );
    }

    #[test]
    fn ancient_worker_never_becomes_current_after_replacements() {
        let readiness = IngestionReadiness::new();
        let ancient = readiness.candidate();
        readiness.install(Arc::clone(&ancient));
        make_ready(&readiness, &ancient);

        for _ in 0..10_000 {
            let current = readiness.candidate();
            readiness.install(Arc::clone(&current));
            assert!(!readiness.is_current(&ancient));
            assert!(!readiness.mark_ready(&ancient, Uuid::new_v4()));
            make_ready(&readiness, &current);
        }
    }

    #[test]
    fn superseded_worker_cannot_publish_or_revoke_current_readiness() {
        let readiness = IngestionReadiness::new();
        let first = readiness.candidate();
        readiness.install(Arc::clone(&first));
        make_ready(&readiness, &first);

        let second = readiness.candidate();
        readiness.install(Arc::clone(&second));
        assert!(!readiness.is_ready());
        assert!(!readiness.mark_ready(&first, Uuid::new_v4()));
        make_ready(&readiness, &second);
        readiness.mark_not_ready(&first);
        assert!(readiness.is_ready());

        readiness.stop();
        assert!(!readiness.mark_ready(&second, Uuid::new_v4()));
        assert!(!readiness.is_ready());
    }

    #[test]
    fn readiness_lease_is_invalidated_by_disconnect_and_reacknowledgement() {
        let readiness = IngestionReadiness::new();
        let worker = readiness.candidate();
        readiness.install(Arc::clone(&worker));
        make_ready(&readiness, &worker);
        let first_lease = readiness.lease().expect("ready worker should lease");

        readiness.mark_not_ready(&worker);
        assert!(!first_lease.is_current(&readiness));
        assert!(!readiness.mark_ready(&worker, Uuid::new_v4()));
        make_ready(&readiness, &worker);
        assert!(!first_lease.is_current(&readiness));
        assert!(
            readiness
                .lease()
                .is_some_and(|lease| lease.is_current(&readiness))
        );
    }

    #[test]
    fn rejected_subscription_cannot_restore_readiness_from_a_buffered_event() {
        let readiness = IngestionReadiness::new();
        let worker = readiness.candidate();
        readiness.install(Arc::clone(&worker));
        let rejected_subscription_id = make_ready(&readiness, &worker);
        let lease = readiness.lease().expect("ready worker should lease");

        readiness.invalidate_lease(&lease, "remote lease rejected".into());

        assert!(!readiness.mark_ready(&worker, rejected_subscription_id));
        assert!(!readiness.is_subscription_current(&worker, rejected_subscription_id));
        assert!(!readiness.is_ready());
        make_ready(&readiness, &worker);
        assert!(readiness.is_ready());
    }

    #[test]
    fn readiness_transition_cannot_interleave_a_leased_commit() {
        let readiness = IngestionReadiness::new();
        let worker = readiness.candidate();
        readiness.install(Arc::clone(&worker));
        make_ready(&readiness, &worker);
        let lease = readiness.lease().expect("ready worker should lease");
        let (commit_entered, entered) = mpsc::channel();
        let (release_commit, released) = mpsc::channel();
        let committing_readiness = readiness.clone();
        let commit = thread::spawn(move || {
            committing_readiness
                .commit_if_current(&lease, || {
                    commit_entered
                        .send(())
                        .expect("test receiver should remain");
                    released.recv().expect("commit should be released");
                    Ok::<(), ()>(())
                })
                .expect("fixture commit should succeed")
        });
        entered.recv().expect("commit should enter");

        let transition_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transitioned = Arc::clone(&transition_done);
        let (transition_started, started) = mpsc::channel();
        let transitioning_readiness = readiness.clone();
        let transition = thread::spawn(move || {
            transition_started
                .send(())
                .expect("test receiver should remain");
            transitioning_readiness.mark_not_ready(&worker);
            transitioned.store(true, std::sync::atomic::Ordering::Release);
        });
        started.recv().expect("transition should start");
        thread::sleep(Duration::from_millis(50));
        assert!(!transition_done.load(std::sync::atomic::Ordering::Acquire));

        release_commit.send(()).expect("commit should be released");
        assert!(commit.join().expect("commit worker should stop"));
        transition.join().expect("transition worker should stop");
        assert!(!readiness.is_ready());
    }

    #[test]
    fn health_combines_backend_and_ingestion_failures() {
        let readiness = IngestionReadiness::new();
        let worker = readiness.candidate();
        readiness.install(Arc::clone(&worker));
        readiness.mark_unavailable(
            &worker,
            "violation persistence failed: database is locked".into(),
        );

        let health = combine_health(
            agentsight_enforcement_protocol::HealthStatus {
                ready: false,
                backend: "actplane".into(),
                capabilities: agentsight_enforcement_protocol::EnforcementCapabilities::actplane(),
                message: Some("violation event buffer overflow: dropped_events=1".into()),
            },
            &readiness,
        );

        assert!(!health.ready);
        assert_eq!(
            health.message.as_deref(),
            Some(
                "violation event buffer overflow: dropped_events=1; violation persistence failed: database is locked"
            )
        );
    }

    fn fixture_policy(binding_id: Uuid) -> ApplyPolicy {
        ApplyPolicy {
            binding_id,
            agent_id: "agent-1".into(),
            session_id: None,
            root_pid: 42,
            process_start_time: 7,
            policy_id: "policy-1".into(),
            policy_revision: "revision-1".into(),
            policy_dsl: "label AGENT".into(),
            policy_mode: None,
        }
    }

    fn fixture_health(ready: bool, message: Option<&str>) -> HealthStatus {
        HealthStatus {
            ready,
            backend: "fixture".into(),
            capabilities:
                agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
            message: message.map(str::to_string),
        }
    }

    /// Serves one response per client connection, mirroring the request-reply UDS protocol.
    fn fixture_enforcer(
        responses: Vec<Result<ResponseBody, RemoteError>>,
    ) -> (EnforcementClient, thread::JoinHandle<()>, PathBuf) {
        let socket_path =
            std::env::temp_dir().join(format!("enforcement-coordinator-{}.sock", Uuid::new_v4()));
        let listener = UnixListener::bind(&socket_path).expect("fixture listener should bind");
        let server = thread::spawn(move || {
            for result in responses {
                let (mut stream, _) = listener.accept().expect("fixture client should connect");
                let request: Request = read_frame(&mut BufReader::new(
                    stream
                        .try_clone()
                        .expect("fixture stream should clone for reading"),
                ))
                .expect("fixture request should decode")
                .expect("fixture request should exist");
                let response = Response {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: request.request_id,
                    result,
                };
                let mut bytes = Vec::new();
                write_frame(&mut bytes, &response).expect("fixture response should encode");
                stream
                    .write_all(&bytes)
                    .expect("fixture response should write");
                stream.flush().expect("fixture response should flush");
            }
        });
        (EnforcementClient::new(&socket_path), server, socket_path)
    }

    #[test]
    fn active_bindings_hide_detached_terminal_state_only() {
        let coordinator = EnforcementCoordinator::new(
            EnforcementClient::new("/tmp/unused-enforcement.sock"),
            EnforcementStore::open(":memory:").expect("test store should open"),
        );
        let states = [
            BindingState::Pending,
            BindingState::Enforced,
            BindingState::Failed,
            BindingState::Degraded,
            BindingState::Detaching,
            BindingState::Detached,
        ];
        for state in states {
            let binding = Binding {
                request: fixture_policy(Uuid::new_v4()),
                state,
                message: None,
                domain_id: None,
            };
            coordinator
                .store
                .upsert_binding(&binding)
                .expect("fixture binding should persist");
        }

        let listed = coordinator
            .active_bindings()
            .expect("active bindings should list");
        assert_eq!(listed.len(), 5);
        assert!(
            listed
                .iter()
                .all(|binding| binding.state != BindingState::Detached)
        );

        // The store keeps detached rows: containment and reconciliation
        // consume the full snapshot through `bindings`.
        assert_eq!(
            coordinator
                .store
                .bindings()
                .expect("store bindings should list")
                .len(),
            6
        );
    }

    #[test]
    fn apply_rejection_persists_failed_state_and_reports_it() {
        let (client, server, socket_path) = fixture_enforcer(vec![
            Ok(ResponseBody::Health(fixture_health(true, None))),
            Err(RemoteError {
                code: "internal_error".into(),
                message: "fixture apply rejection".into(),
            }),
        ]);
        let coordinator = EnforcementCoordinator::new(
            client,
            EnforcementStore::open(":memory:").expect("test store should open"),
        );
        let worker = coordinator.ingestion_readiness.candidate();
        coordinator.ingestion_readiness.install(Arc::clone(&worker));
        make_ready(&coordinator.ingestion_readiness, &worker);

        let binding_id = Uuid::new_v4();
        let result = coordinator.apply(fixture_policy(binding_id));
        server.join().expect("fixture server should join");
        std::fs::remove_file(&socket_path).expect("fixture socket should be removed");

        match result {
            Err(EnforcementCoordinatorError::PersistedUnacknowledged {
                binding_id: persisted_id,
                state,
                ..
            }) => {
                assert_eq!(persisted_id, binding_id);
                assert_eq!(state, BindingState::Failed);
            }
            other => panic!("expected persisted-unacknowledged failure, got {other:?}"),
        }
        let stored = coordinator
            .store
            .binding(binding_id)
            .expect("store read should succeed")
            .expect("rejected binding should stay persisted");
        assert_eq!(stored.state, BindingState::Failed);
        assert!(
            coordinator
                .active_bindings()
                .expect("active bindings should list")
                .iter()
                .any(|binding| binding.request.binding_id == binding_id)
        );
    }

    #[test]
    fn apply_degraded_after_backend_loss_reports_persisted_state() {
        let request = fixture_policy(Uuid::new_v4());
        let applied = Binding {
            request: request.clone(),
            state: BindingState::Enforced,
            message: None,
            domain_id: None,
        };
        let (client, server, socket_path) = fixture_enforcer(vec![
            Ok(ResponseBody::Health(fixture_health(true, None))),
            Ok(ResponseBody::Applied(applied)),
            Ok(ResponseBody::Health(fixture_health(
                false,
                Some("fixture backend lost"),
            ))),
        ]);
        let coordinator = EnforcementCoordinator::new(
            client,
            EnforcementStore::open(":memory:").expect("test store should open"),
        );
        let worker = coordinator.ingestion_readiness.candidate();
        coordinator.ingestion_readiness.install(Arc::clone(&worker));
        make_ready(&coordinator.ingestion_readiness, &worker);

        let result = coordinator.apply(request);
        server.join().expect("fixture server should join");
        std::fs::remove_file(&socket_path).expect("fixture socket should be removed");

        match result {
            Err(EnforcementCoordinatorError::PersistedUnacknowledged {
                binding_id, state, ..
            }) => {
                assert_eq!(state, BindingState::Degraded);
                let stored = coordinator
                    .store
                    .binding(binding_id)
                    .expect("store read should succeed")
                    .expect("degraded binding should stay persisted");
                assert_eq!(stored.state, BindingState::Degraded);
            }
            other => panic!("expected persisted-unacknowledged failure, got {other:?}"),
        }
    }
}
