//! Target-specific composition; queue and lifecycle remain in Policy Runtime.
use asc_agentsight_client::AgentSightClientFactory;
use asc_pcp::{
    BindingReconciler, RetryPolicy, StoreError, TargetDeploymentClient,
    TargetDeploymentClientFactory,
};
use asc_policy_repository::{BindingReconcileCatalog, BindingStateRepository};
use asc_policy_runtime::reconciliation::{MonotonicClock, ReconciliationRuntime, RuntimeConfig};
use asc_policy_translator_agentsight::AgentSightTranslator;
use asc_policy_types::Validate;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::target::{
    AdapterFault, Failure, FailureKind, TargetBindingPlan, TranslationOutcome,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Composes file and credential execution routes without coupling their readiness.
/// # Errors
/// Returns repository readiness or worker startup failures.
pub fn start_policy_reconciliation_with_gateway<
    R: BindingStateRepository + BindingReconcileCatalog + 'static,
>(
    repository: Arc<R>,
    gateway: Option<Arc<asc_gateway_runtime::GatewayService>>,
) -> Result<ReconciliationRuntime, StoreError> {
    let factory: Arc<dyn TargetDeploymentClientFactory> = Arc::new(move || {
        let service = gateway
            .as_ref()
            .ok_or_else(|| Failure::new(FailureKind::Retryable, "GATEWAY_UNAVAILABLE"))?;
        Ok(Arc::new(asc_gateway_runtime::GatewayBindingClient::new(
            service.clone(),
        )) as Arc<dyn TargetDeploymentClient>)
    });
    start_with_factories(
        repository,
        Arc::new(AgentSightClientFactory::default()),
        Some(factory),
    )
}

/// Uses an injected file target Client for component validation.
/// # Errors
/// Returns repository readiness or worker startup failures.
pub fn start_policy_reconciliation_with_client<
    R: BindingStateRepository + BindingReconcileCatalog + 'static,
>(
    repository: Arc<R>,
    client: Arc<dyn TargetDeploymentClient>,
) -> Result<ReconciliationRuntime, StoreError> {
    start_with_factories(repository, Arc::new(move || Ok(client.clone())), None)
}

fn start_with_factories<R: BindingStateRepository + BindingReconcileCatalog + 'static>(
    repository: Arc<R>,
    file_factory: Arc<dyn TargetDeploymentClientFactory>,
    gateway_factory: Option<Arc<dyn TargetDeploymentClientFactory>>,
) -> Result<ReconciliationRuntime, StoreError> {
    let clock = Arc::new(MonotonicClock::default());
    let mut factories = BTreeMap::from([("agentsight".into(), file_factory)]);
    if let Some(factory) = gateway_factory {
        factories.insert("gateway".into(), factory);
    }
    let adapter = |binding: &PreparedBinding| {
        if binding.policy.template.uses_credentials() {
            binding.validate().map_err(|_| AdapterFault {
                code: "GATEWAY_INVALID_BINDING".into(),
            })?;
            let content = serde_json::to_vec(binding).map_err(|_| AdapterFault {
                code: "GATEWAY_ENCODING_FAILED".into(),
            })?;
            Ok(TranslationOutcome::Translated(TargetBindingPlan {
                format: "agent-sec-gateway-binding/v1".into(),
                content,
            }))
        } else {
            AgentSightTranslator.translate(binding)
        }
    };
    let core = Arc::new(
        BindingReconciler::new(
            repository.clone(),
            Arc::new(adapter),
            factories,
            "agentsight".into(),
            clock,
            RetryPolicy {
                max_attempts: 5,
                base_delay_ms: 1000,
                max_delay_ms: 30_000,
            },
        )?
        .with_route_selector(|binding| {
            if binding.policy.template.uses_credentials() {
                "gateway".into()
            } else {
                "agentsight".into()
            }
        }),
    );
    ReconciliationRuntime::start(repository, core, RuntimeConfig::default())
}
