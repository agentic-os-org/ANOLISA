//! Observe claim, dispatch and query behavior without executing an external action.

use std::collections::BTreeSet;
use std::sync::Arc;

use semantic_arbiter::composition::{CapabilityRegistry, DirectiveCapability};
use semantic_arbiter::contract::Binding;
use semantic_arbiter::executor::fixture::{InMemoryClaimStore, ScriptedExecutor};
use semantic_arbiter::executor::{AuthorizationContext, EffectState, ExecutorRegistry};
use semantic_arbiter::outcome::{Directive, Obligation, ObligationAction};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binding = Binding {
        session_id: "session-example".into(),
        instance_id: "instance-example".into(),
        workspace_id: "workspace-example".into(),
        task_version: "task-v1".into(),
        config_revision: "policy-v1".into(),
        candidate_id: "candidate-example".into(),
        candidate_revision: "candidate-v1".into(),
    };
    let authorization = AuthorizationContext {
        api_version: "semantic-arbiter/v1alpha1".into(),
        request_id: "request-example".into(),
        binding,
        expires_at_ms: 2_000,
        capabilities: CapabilityRegistry::new([DirectiveCapability::Audit]),
        allowed_ids: BTreeSet::new(),
    };
    let directive = Directive::Obligation(Obligation {
        id: "audit".into(),
        critical: true,
        action: ObligationAction::Audit {},
    });
    let executor = Arc::new(ScriptedExecutor::new(
        DirectiveCapability::Audit,
        EffectState::Unconfirmed,
    ));
    let mut registry = ExecutorRegistry::new(Arc::new(InMemoryClaimStore::default()));
    registry.register(executor.clone())?;

    let first = registry.execute(&authorization, &directive, 1_000).await?;
    let second = registry.execute(&authorization, &directive, 1_001).await?;
    println!(
        "first={:?} second={:?} dispatches={} queries={}",
        first.receipt.state,
        second.receipt.state,
        executor.dispatch_count(),
        executor.query_count()
    );
    Ok(())
}
