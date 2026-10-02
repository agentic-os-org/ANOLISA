//! Target-specific composition only; queue and lifecycle implementation live in Policy Runtime.
use asc_agentsight_client::AgentSightClientFactory;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_pcp::{
    BindingReconciler, RetryPolicy, StoreError, TargetDeploymentClient,
    TargetDeploymentClientFactory,
};
use asc_policy_adapter_agentsight::AgentSightAdapter;
use asc_policy_runtime::reconciliation::{MonotonicClock, ReconciliationRuntime, RuntimeConfig};
use asc_policy_types::binding::PreparedBinding;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Initializes the service's PEP dependencies and starts policy reconciliation.
/// Target selection and construction belong here, not in the process entrypoint.
/// # Errors
/// Returns a Repository readiness or worker startup error.
pub fn start_policy_reconciliation(
    repository: Arc<ProcessLocalPapRepository>,
) -> Result<ReconciliationRuntime, StoreError> {
    start_policy_reconciliation_with_factory(
        repository,
        Arc::new(AgentSightClientFactory::default()),
    )
}

/// Starts the `AgentSight` composition with an injected deployment Client.
/// This permits component validation without host credentials or target I/O.
/// # Errors
/// Returns a core configuration, Repository readiness or worker startup error.
pub fn start_policy_reconciliation_with_client(
    repository: Arc<ProcessLocalPapRepository>,
    client: Arc<dyn TargetDeploymentClient>,
) -> Result<ReconciliationRuntime, StoreError> {
    start_policy_reconciliation_with_factory(repository, Arc::new(move || Ok(client.clone())))
}

fn start_policy_reconciliation_with_factory(
    repository: Arc<ProcessLocalPapRepository>,
    factory: Arc<dyn TargetDeploymentClientFactory>,
) -> Result<ReconciliationRuntime, StoreError> {
    let clock = Arc::new(MonotonicClock::default());
    let adapter = |binding: &PreparedBinding| AgentSightAdapter.translate(binding);
    let core = Arc::new(BindingReconciler::new(
        repository.clone(),
        Arc::new(adapter),
        BTreeMap::from([("agentsight".into(), factory)]),
        "agentsight".into(),
        clock,
        RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
        },
    )?);
    ReconciliationRuntime::start(repository, core, RuntimeConfig::default())
}

/// Failed service admission remains explicit while unrelated daemon services run.
pub struct UnavailableReconciliation;

impl asc_pap::BindingReconcileEnqueuer for UnavailableReconciliation {
    fn check_ready(&self) -> Result<(), asc_pap::PapError> {
        Err(asc_pap::PapError::Unavailable)
    }

    fn enqueue(
        &self,
        _: &asc_policy_types::identifiers::ResourceId,
    ) -> Result<(), asc_pap::EnqueueError> {
        Err(asc_pap::EnqueueError::Stopped)
    }
}
