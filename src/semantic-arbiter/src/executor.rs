//! Consumer-side admission and idempotent dispatch contracts for typed directives.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::composition::{CapabilityRegistry, DirectiveCapability};
use crate::contract::Binding;
use crate::evaluation::EVALUATION_API_VERSION;
use crate::outcome::{Directive, ObligationAction, Receipt, ReceiptReason, ReceiptState};
use crate::protocol::CHECK_API_VERSION;

/// Pure in-memory fixtures for contract tests and examples.
pub mod fixture;

/// Trusted consumer authorization, separate from evidence and model output.
#[derive(Clone, Debug)]
pub struct AuthorizationContext {
    /// Protocol version retained with the original directive.
    pub api_version: String,
    /// Consumer-owned immutable request identity.
    pub request_id: String,
    /// Full task, candidate, workspace and policy association.
    pub binding: Binding,
    /// Exclusive authorization deadline in Unix milliseconds.
    pub expires_at_ms: u64,
    /// Capabilities explicitly authorized for this request.
    pub capabilities: CapabilityRegistry,
    /// Controlled identifiers authorized for subject scopes and action parameters.
    pub allowed_ids: BTreeSet<String>,
}

/// Immutable request passed to exactly one registered executor.
#[derive(Clone, Debug)]
pub struct ExecutionRequest {
    /// Domain-separated key used to claim or query this request.
    pub idempotency_key: String,
    /// Exact protocol version of the originating directive.
    pub api_version: String,
    /// Consumer-owned request identity.
    pub request_id: String,
    /// Full immutable binding.
    pub binding: Binding,
    /// Typed directive containing no executable text.
    pub directive: Directive,
}

/// Independently observed effect after dispatch or a prior claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectState {
    /// Target effect was independently confirmed.
    Confirmed,
    /// Target effect was independently confirmed absent or denied.
    Denied,
    /// Effect cannot currently be established.
    Unconfirmed,
}

/// Stable executor failures retained without provider bodies or secrets.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExecutorFailure {
    /// Dispatch did not complete with a known effect.
    #[error("dispatch_failed")]
    DispatchFailed,
    /// Effect lookup did not produce a reliable result.
    #[error("query_failed")]
    QueryFailed,
    /// Idempotency storage was unavailable.
    #[error("claim_failed")]
    ClaimFailed,
}

/// A consumer implementation for one granular directive capability.
pub trait Executor: Send + Sync {
    /// Exact directive capability supported by this implementation.
    fn capability(&self) -> DirectiveCapability;

    /// Dispatch once after a successful new claim; success proves no effect.
    fn dispatch<'a>(
        &'a self,
        request: &'a ExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutorFailure>> + Send + 'a>>;

    /// Independently query the effect of the original immutable request.
    fn query<'a>(
        &'a self,
        request: &'a ExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<EffectState, ExecutorFailure>> + Send + 'a>>;
}

/// Result of atomically claiming an idempotency key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimResult {
    /// This caller owns the first dispatch attempt.
    New,
    /// A prior caller already claimed the immutable request.
    Existing,
}

/// Consumer-owned persistence for idempotency claims.
pub trait ClaimStore: Send + Sync {
    /// Atomically create a claim or report that it already exists.
    fn claim(&self, idempotency_key: &str) -> Result<ClaimResult, ExecutorFailure>;
}

/// Admission and registry failures occur before any new dispatch.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExecutionError {
    /// The trusted consumer context did not authorize the directive.
    #[error("admission_denied")]
    AdmissionDenied,
    /// No executor was explicitly registered for the required capability.
    #[error("executor_unavailable")]
    ExecutorUnavailable,
    /// More than one executor attempted to own one capability.
    #[error("executor_conflict")]
    ExecutorConflict,
    /// The immutable request could not be serialized for correlation.
    #[error("invalid_directive")]
    InvalidDirective,
    /// The idempotency claim could not be established safely.
    #[error("claim_unavailable")]
    ClaimUnavailable,
}

/// Receipt plus whether this invocation attempted a new dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionReport {
    /// Consumer receipt derived from independent effect observation.
    pub receipt: Receipt,
    /// True only for the invocation that obtained a new claim.
    pub dispatched: bool,
}

/// Explicit process-local executor registry and shared claim store.
pub struct ExecutorRegistry {
    executors: BTreeMap<DirectiveCapability, Arc<dyn Executor>>,
    claims: Arc<dyn ClaimStore>,
}

impl ExecutorRegistry {
    /// Create an empty registry over consumer-owned claim persistence.
    pub fn new(claims: Arc<dyn ClaimStore>) -> Self {
        Self {
            executors: BTreeMap::new(),
            claims,
        }
    }

    /// Register exactly one implementation for its declared capability.
    pub fn register(&mut self, executor: Arc<dyn Executor>) -> Result<(), ExecutionError> {
        let capability = executor.capability();
        if self.executors.contains_key(&capability) {
            return Err(ExecutionError::ExecutorConflict);
        }
        self.executors.insert(capability, executor);
        Ok(())
    }

    /// Admit, claim, dispatch at most once, then independently query the effect.
    pub async fn execute(
        &self,
        authorization: &AuthorizationContext,
        directive: &Directive,
        now_ms: u64,
    ) -> Result<ExecutionReport, ExecutionError> {
        let capability = DirectiveCapability::for_directive(directive);
        admit(authorization, directive, capability, now_ms)?;
        let executor = self
            .executors
            .get(&capability)
            .ok_or(ExecutionError::ExecutorUnavailable)?;
        let idempotency_key = idempotency_key(
            &authorization.api_version,
            &authorization.request_id,
            &authorization.binding,
            directive,
        )?;
        let request = ExecutionRequest {
            idempotency_key: idempotency_key.clone(),
            api_version: authorization.api_version.clone(),
            request_id: authorization.request_id.clone(),
            binding: authorization.binding.clone(),
            directive: directive.clone(),
        };
        let claim = self
            .claims
            .claim(&idempotency_key)
            .map_err(|_| ExecutionError::ClaimUnavailable)?;
        let dispatched = claim == ClaimResult::New;
        if dispatched {
            let _ = executor.dispatch(&request).await;
        }
        let effect = executor
            .query(&request)
            .await
            .unwrap_or(EffectState::Unconfirmed);
        let (state, reason) = match effect {
            EffectState::Confirmed => (ReceiptState::Applied, ReceiptReason::Confirmed),
            EffectState::Denied => (ReceiptState::Rejected, ReceiptReason::Denied),
            EffectState::Unconfirmed => (ReceiptState::Unknown, ReceiptReason::Unconfirmed),
        };
        Ok(ExecutionReport {
            receipt: Receipt {
                api_version: authorization.api_version.clone(),
                request_id: authorization.request_id.clone(),
                binding: authorization.binding.clone(),
                directive_id: directive.id().into(),
                state,
                reason,
            },
            dispatched,
        })
    }
}

/// Compute the stable correlation key for one immutable directive.
pub fn idempotency_key(
    api_version: &str,
    request_id: &str,
    binding: &Binding,
    directive: &Directive,
) -> Result<String, ExecutionError> {
    let bytes = serde_json::to_vec(&(api_version, request_id, binding, directive))
        .map_err(|_| ExecutionError::InvalidDirective)?;
    let mut hash = Sha256::new();
    hash.update(b"semantic-arbiter\0executor-directive\0");
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn admit(
    authorization: &AuthorizationContext,
    directive: &Directive,
    capability: DirectiveCapability,
    now_ms: u64,
) -> Result<(), ExecutionError> {
    if !matches!(
        authorization.api_version.as_str(),
        CHECK_API_VERSION | EVALUATION_API_VERSION
    ) || authorization.request_id.is_empty()
        || directive.id().is_empty()
        || now_ms >= authorization.expires_at_ms
        || !authorization.capabilities.contains(capability)
    {
        return Err(ExecutionError::AdmissionDenied);
    }
    match directive {
        Directive::Approval { candidate_id, .. } => {
            if candidate_id != &authorization.binding.candidate_id {
                return Err(ExecutionError::AdmissionDenied);
            }
        }
        Directive::Subject(item) => {
            if item.instance_id != authorization.binding.instance_id
                || item.scope_ids.is_empty()
                || !item
                    .scope_ids
                    .iter()
                    .all(|id| !id.is_empty() && authorization.allowed_ids.contains(id))
            {
                return Err(ExecutionError::AdmissionDenied);
            }
        }
        Directive::Obligation(item) => match &item.action {
            ObligationAction::Audit {} => {}
            ObligationAction::Snapshot { workspace_id } => {
                require_workspace(authorization, workspace_id)?;
            }
            ObligationAction::Rollback {
                workspace_id,
                checkpoint_id,
            } => {
                require_workspace(authorization, workspace_id)?;
                require_ids(authorization, std::slice::from_ref(checkpoint_id))?;
            }
            ObligationAction::Cleanup {
                workspace_id,
                checkpoint_ids,
            } => {
                require_workspace(authorization, workspace_id)?;
                require_ids(authorization, checkpoint_ids)?;
            }
            ObligationAction::Notify { recipient_id } => {
                require_ids(authorization, std::slice::from_ref(recipient_id))?;
            }
            ObligationAction::Diagnose { diagnostic_id } => {
                require_ids(authorization, std::slice::from_ref(diagnostic_id))?;
            }
            ObligationAction::RetrieveSkill { query_id }
            | ObligationAction::RetrieveMemory { query_id } => {
                require_ids(authorization, std::slice::from_ref(query_id))?;
            }
            ObligationAction::InjectContext { material_ids } => {
                require_ids(authorization, material_ids)?;
            }
        },
    }
    Ok(())
}

fn require_workspace(
    authorization: &AuthorizationContext,
    workspace_id: &str,
) -> Result<(), ExecutionError> {
    if workspace_id != authorization.binding.workspace_id {
        return Err(ExecutionError::AdmissionDenied);
    }
    Ok(())
}

fn require_ids(authorization: &AuthorizationContext, ids: &[String]) -> Result<(), ExecutionError> {
    if ids.is_empty()
        || !ids
            .iter()
            .all(|id| !id.is_empty() && authorization.allowed_ids.contains(id))
    {
        return Err(ExecutionError::AdmissionDenied);
    }
    Ok(())
}
