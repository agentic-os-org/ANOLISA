//! Case-level orchestration for upgrading audit evidence to enforcement.

pub(super) mod enforcer;
mod pending;
mod policy;
mod reconciler;
mod rollback;
mod worker;

use std::sync::Arc;
use std::time::Duration;

use agentsight_enforcement_protocol::{ReplacePolicy, ReplacementPolicy, ReplacementSource};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use self::enforcer::{ContainmentEnforcer, ContainmentEnforcerError};
use self::pending::{now_ns, record_attach_failed, record_direct_pending_unavailable};
use self::policy::{
    ResolvedPolicy, acknowledgement_matches, enforce_request, exact_binding, existing_action,
    live_candidates, live_lifecycle, resolve_policy, resolve_transition_policy, sanitize_failure,
    select_candidate, source_policy_snapshot, validate_duration, validate_process_identity,
    validate_requested_by,
};
use super::{
    ContainmentAction, ContainmentActivationResult, ContainmentClaimResult,
    ContainmentFailureStage, ContainmentLifecycle, RiskCaseDetail, RiskCaseStatus, SecurityStore,
    SecurityStoreError,
};
use crate::enforcement::{TransitionDirection, TransitionKey, read_process_start_time};

const DEFAULT_DURATION_SECS: u64 = 900;
const MIN_DURATION_SECS: u64 = 60;
const MAX_DURATION_SECS: u64 = 86_400;

/// User-selected process and duration for one containment request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentRequest {
    /// Selected process-tree root.
    pub root_pid: i32,
    /// Temporary duration, or `None` for explicit persistent enforcement.
    pub duration_secs: Option<u64>,
}

/// Live process identity eligible to receive the case-derived policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentCandidate {
    /// Product Agent identifier.
    pub agent_id: String,
    /// Candidate process-tree root.
    pub root_pid: i32,
    /// Linux process start time paired with `root_pid`.
    pub process_start_time: u64,
    /// Human-readable Agent or process name.
    pub display_name: String,
}

/// Case-derived data needed to confirm a containment request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentPlan {
    /// Source audit case.
    pub case_id: Uuid,
    /// Canonical source path recovered from the original persisted binding.
    pub source_path: String,
    /// Process identity from the original binding.
    pub original_target: Option<ContainmentCandidate>,
    /// Whether the original PID still has its recorded start time.
    pub original_target_valid: bool,
    /// Current same-Agent replacement candidates.
    pub candidates: Vec<ContainmentCandidate>,
    /// Safe temporary duration shown by the confirmation UI.
    pub default_duration_secs: u64,
    /// Smallest accepted temporary duration.
    pub min_duration_secs: u64,
    /// Largest accepted temporary duration.
    pub max_duration_secs: u64,
    /// Most recent action, when one already exists.
    pub existing_action: Option<ContainmentAction>,
}

/// Typed failures at the case, process, store, and enforcer boundaries.
#[derive(Debug, Error)]
pub enum ContainmentError {
    /// The requested case does not exist.
    #[error("risk case {0} does not exist")]
    MissingCase(Uuid),
    /// Original policy provenance cannot safely produce an enforce policy.
    #[error("source policy for case {0} is unavailable")]
    SourcePolicyUnavailable(Uuid),
    /// The selected PID is missing, protected, recycled, or not an approved candidate.
    #[error("root process {0} is stale")]
    RootProcessStale(i32),
    /// Multiple trusted candidates identify the same selected PID.
    #[error("multiple trusted candidates identify root process {0}")]
    AmbiguousCandidate(i32),
    /// The case review state forbids containment.
    #[error("case {case_id} cannot be contained from state {status:?}")]
    IneligibleCase {
        /// Requested case.
        case_id: Uuid,
        /// Current ineligible review state.
        status: RiskCaseStatus,
    },
    /// A temporary duration falls outside the approved bounds.
    #[error("duration must be null or between 60 and 86400 seconds")]
    InvalidDuration,
    /// The authenticated requester identity is unsafe to persist.
    #[error("requested_by must be 1 to 128 bytes without control characters")]
    InvalidRequestedBy,
    /// An active lifecycle action exists with a different target or duration.
    #[error("containment action {0} already targets this case with different parameters")]
    IncompatibleAction(Uuid),
    /// A compatible action is still awaiting an enforcement acknowledgement.
    #[error("containment action {0} is still in progress")]
    ContainmentInProgress(Uuid),
    /// A compatible action is already detaching and cannot be reported active.
    #[error("containment action {0} is expiring")]
    ContainmentExpiring(Uuid),
    /// Human review made the case ineligible while enforcement was applying.
    #[error("case {case_id} changed to {status:?} while containment was applying")]
    CaseEligibilityChanged {
        /// Requested case.
        case_id: Uuid,
        /// Review state observed by the activation transaction.
        status: RiskCaseStatus,
    },
    /// Detachment failed, so the binding may still enforce until reconciliation succeeds.
    #[error(
        "containment action {action_id} requires cleanup; binding {binding_id} may remain attached: {reason}"
    )]
    CleanupRequired {
        /// Action retaining the case-level uniqueness claim.
        action_id: Uuid,
        /// Binding whose attachment state is uncertain.
        binding_id: Uuid,
        /// Sanitized detachment failure detail.
        reason: String,
    },
    /// The privileged enforcement boundary failed or returned an invalid acknowledgement.
    #[error("enforcement unavailable: {0}")]
    Enforcer(String),
    /// Local security persistence failed.
    #[error(transparent)]
    Store(#[from] SecurityStoreError),
    /// A second background reconciler was requested for this coordinator.
    #[error("containment reconciler is already running")]
    AlreadyRunning,
    /// Spawning the background reconciler failed.
    #[error("failed to start containment reconciler: {0}")]
    ReconcilerThread(std::io::Error),
    /// Persisted provenance could not safely restore a pending action.
    #[error("containment action {action_id} recovery failed: {reason}")]
    RecoveryFailed {
        /// Pending action that could not be recovered.
        action_id: Uuid,
        /// Sanitized actionable failure detail.
        reason: String,
    },
    /// Another worker replaced the lifecycle claim while an operation was in flight.
    #[error("containment action {0} reconciliation claim was lost")]
    ClaimLost(Uuid),
    /// Malformed due rows were quarantined after valid rows continued.
    #[error("quarantined {count} corrupt containment action(s)")]
    CorruptActions {
        /// Number of malformed rows quarantined from this bounded batch.
        count: usize,
    },
}

/// Coordinates provenance recovery, durable intent, and enforced acknowledgement.
pub struct ContainmentCoordinator {
    store: Arc<SecurityStore>,
    enforcer: Arc<dyn ContainmentEnforcer>,
    worker: Arc<worker::ReconcilerControl>,
}

impl ContainmentCoordinator {
    /// Creates a coordinator without starting background reconciliation.
    pub fn new(store: Arc<SecurityStore>, enforcer: Arc<dyn ContainmentEnforcer>) -> Self {
        Self {
            store,
            enforcer,
            worker: Arc::new(worker::ReconcilerControl::new()),
        }
    }

    /// Builds a containment plan solely from the case's persisted binding provenance.
    ///
    /// # Errors
    /// Returns a typed case, policy-provenance, store, or enforcer error.
    pub fn plan(
        &self,
        case_id: Uuid,
        candidates: Vec<ContainmentCandidate>,
    ) -> Result<ContainmentPlan, ContainmentError> {
        let context = self.case_context(case_id)?;
        let candidates = live_candidates(&context.detail.case.agent_id, candidates)?;
        let request = &context.binding.request;
        let original_target = ContainmentCandidate {
            agent_id: request.agent_id.clone(),
            root_pid: request.root_pid,
            process_start_time: request.process_start_time,
            display_name: context
                .detail
                .evidence
                .iter()
                .find_map(|event| event.identity.agent_name.clone())
                .unwrap_or_else(|| request.agent_id.clone()),
        };
        let original_target_valid = read_process_start_time(original_target.root_pid)
            .is_ok_and(|actual| actual == original_target.process_start_time);
        Ok(ContainmentPlan {
            case_id,
            source_path: context.source_path,
            original_target: Some(original_target),
            original_target_valid,
            candidates,
            default_duration_secs: DEFAULT_DURATION_SECS,
            min_duration_secs: MIN_DURATION_SECS,
            max_duration_secs: MAX_DURATION_SECS,
            existing_action: self.store.latest_containment_action(case_id)?,
        })
    }

    /// Persists pending intent, applies enforcement, and confirms after acknowledgement.
    ///
    /// `None` is an explicit persistent duration; no request-side default is applied.
    ///
    /// # Errors
    /// Returns a typed validation, case, policy-provenance, store, or enforcer error.
    pub fn contain(
        &self,
        case_id: Uuid,
        request: ContainmentRequest,
        candidates: &[ContainmentCandidate],
        requested_by: &str,
    ) -> Result<ContainmentAction, ContainmentError> {
        validate_duration(request.duration_secs)?;
        let requested_by = validate_requested_by(requested_by)?;
        // Idempotent replay must run before the eligibility gate: activating a
        // containment auto-resolves its case, so a repeated request for an
        // already-live action must be returned as-is rather than rejected for the
        // now-ineligible (resolved) case. Case eligibility is only required when
        // creating a brand-new containment below.
        if let Some(existing) = self.store.latest_containment_action(case_id)?
            && live_lifecycle(existing.lifecycle_state)
        {
            if existing.root_pid != request.root_pid
                || existing.duration_secs != request.duration_secs
            {
                return Err(ContainmentError::IncompatibleAction(existing.action_id));
            }
            let process_start_time =
                validate_process_identity(existing.root_pid, existing.process_start_time)?;
            return existing_action(existing, &request, process_start_time);
        }
        let detail = self.case_detail(case_id)?;
        let context = self.context_from_detail(detail)?;
        let process_start_time = if request.root_pid == context.binding.request.root_pid
            && validate_process_identity(
                request.root_pid,
                context.binding.request.process_start_time,
            )
            .is_ok()
        {
            context.binding.request.process_start_time
        } else {
            select_candidate(&context.detail.case.agent_id, request.root_pid, candidates)?
                .process_start_time
        };

        let now = now_ns();
        let claim_lease_ns = duration_ns(self.enforcer.foreground_claim_lease());
        let binding_id = Uuid::new_v4();
        let apply = enforce_request(&context, binding_id, request.root_pid, process_start_time)
            .ok_or(ContainmentError::SourcePolicyUnavailable(case_id))?;
        let source_policy = source_policy_snapshot(&context);
        let expires_at_ns = request
            .duration_secs
            .map(|duration| now.saturating_add(duration.saturating_mul(1_000_000_000)));
        let mut action = ContainmentAction {
            action_id: Uuid::new_v4(),
            case_id,
            binding_id,
            source_binding_id: Some(context.binding.request.binding_id),
            agent_id: context.detail.case.agent_id.clone(),
            root_pid: request.root_pid,
            process_start_time,
            source_path: context.source_path,
            duration_secs: request.duration_secs,
            expires_at_ns,
            lifecycle_state: ContainmentLifecycle::Pending,
            blocked_at_ns: None,
            requested_by,
            failure_stage: None,
            failure_reason: None,
            attempt_count: 0,
            next_retry_at_ns: Some(now.saturating_add(claim_lease_ns)),
            created_at_ns: now,
            updated_at_ns: now,
        };
        match self.store.claim_containment_action(&action)? {
            ContainmentClaimResult::Claimed => {}
            ContainmentClaimResult::Existing(existing) => {
                return existing_action(*existing, &request, process_start_time);
            }
            ContainmentClaimResult::CaseIneligible(status) => {
                return Err(ContainmentError::IneligibleCase { case_id, status });
            }
        }

        let transition_key = TransitionKey {
            action_id: action.action_id,
            direction: TransitionDirection::Forward,
        };
        let stamped = match self.enforcer.begin_transition(
            transition_key,
            ReplacePolicy {
                expected: context.binding.clone(),
                source: ReplacementSource::Credential(source_policy),
                replacement: ReplacementPolicy::Credential(apply.clone()),
            },
        ) {
            Ok(stamped) => stamped,
            Err(ContainmentEnforcerError::MissingTransition(_)) => {
                return record_direct_pending_unavailable(
                    &self.store,
                    action,
                    now_ns(),
                    "persisted policy transition disappeared",
                );
            }
            Err(ContainmentEnforcerError::Unavailable(message)) => {
                return record_direct_pending_unavailable(&self.store, action, now_ns(), &message);
            }
            Err(ContainmentEnforcerError::Rejected(message)) => {
                return record_attach_failed(&self.store, action, now_ns(), &message);
            }
        };
        let (acknowledgement, readiness_stamp) = stamped.into_parts();
        if !acknowledgement_matches(&acknowledgement, &apply) {
            let message = "enforcer returned an invalid binding acknowledgement";
            self.detach_and_fail(&mut action, ContainmentFailureStage::Attach, message)?;
            return Err(ContainmentError::Enforcer(message.into()));
        }

        let claimed_at_ns = action.updated_at_ns;
        let activated_at_ns = now_ns().max(claimed_at_ns.saturating_add(1));
        let lease = match self.enforcer.lease_ready(readiness_stamp) {
            Ok(lease) => lease,
            Err(error) => {
                return record_direct_pending_unavailable(
                    &self.store,
                    action,
                    now_ns(),
                    &error.to_string(),
                );
            }
        };
        let activation = self.store.activate_containment_action(
            action.action_id,
            claimed_at_ns,
            activated_at_ns,
        );
        drop(lease);
        match activation {
            Ok(ContainmentActivationResult::Activated) => {
                action.lifecycle_state = ContainmentLifecycle::Active;
                action.next_retry_at_ns = None;
                action.updated_at_ns = activated_at_ns;
                Ok(action)
            }
            Ok(ContainmentActivationResult::CaseIneligible(status)) => {
                self.detach_and_fail(
                    &mut action,
                    ContainmentFailureStage::Reconcile,
                    "case eligibility changed while enforcement was applying",
                )?;
                Err(ContainmentError::CaseEligibilityChanged { case_id, status })
            }
            Ok(ContainmentActivationResult::LostClaim) => {
                Err(ContainmentError::ClaimLost(action.action_id))
            }
            Err(error) => {
                self.detach_and_fail(
                    &mut action,
                    ContainmentFailureStage::Reconcile,
                    &format!("transactional activation failed: {error}"),
                )?;
                Err(error.into())
            }
        }
    }

    /// Restores the audit policy before removing one live containment binding.
    ///
    /// Returns `false` when the binding does not belong to a live containment
    /// action, allowing the generic enforcement endpoint to handle it.
    ///
    /// # Errors
    ///
    /// Returns a persistence, ownership, or privileged-enforcer failure.
    pub fn remove_binding(&self, binding_id: Uuid) -> Result<bool, ContainmentError> {
        let Some(action) = self.store.containment_action_by_binding(binding_id)? else {
            return Ok(false);
        };
        if action.lifecycle_state == ContainmentLifecycle::Expired {
            return Ok(true);
        }
        if action.lifecycle_state == ContainmentLifecycle::Failed {
            return Err(ContainmentError::CleanupRequired {
                action_id: action.action_id,
                binding_id,
                reason: action
                    .failure_reason
                    .unwrap_or_else(|| "audit restoration exhausted its retry budget".into()),
            });
        }
        let current_time_ns = now_ns();
        if !self
            .store
            .request_containment_expiry(binding_id, current_time_ns)?
        {
            return Err(ContainmentError::ClaimLost(action.action_id));
        }
        self.reconcile_action(action.action_id, current_time_ns)?;
        let restored = self
            .store
            .containment_action(action.action_id)?
            .ok_or(ContainmentError::ClaimLost(action.action_id))?;
        if restored.lifecycle_state != ContainmentLifecycle::Expired {
            return Err(ContainmentError::CleanupRequired {
                action_id: action.action_id,
                binding_id,
                reason: restored.failure_reason.unwrap_or_else(|| {
                    "requested containment action did not restore audit ownership".into()
                }),
            });
        }
        Ok(true)
    }

    fn case_context(&self, case_id: Uuid) -> Result<ResolvedPolicy, ContainmentError> {
        let detail = self.case_detail(case_id)?;
        self.context_from_detail(detail)
    }

    fn case_detail(&self, case_id: Uuid) -> Result<RiskCaseDetail, ContainmentError> {
        let detail = match self.store.case_detail(case_id) {
            Ok(detail) => detail,
            Err(SecurityStoreError::MissingCase(_)) => {
                return Err(ContainmentError::MissingCase(case_id));
            }
            Err(error) => return Err(error.into()),
        };
        if !matches!(
            detail.case.status,
            RiskCaseStatus::Open | RiskCaseStatus::Confirmed
        ) {
            return Err(ContainmentError::IneligibleCase {
                case_id,
                status: detail.case.status,
            });
        }
        Ok(detail)
    }

    fn context_from_detail(
        &self,
        detail: RiskCaseDetail,
    ) -> Result<ResolvedPolicy, ContainmentError> {
        let case_id = detail.case.case_id;
        let snapshot = self
            .enforcer
            .bindings()
            .map_err(|error| ContainmentError::Enforcer(sanitize_failure(&error.to_string())))?;
        let (bindings, _) = snapshot.into_parts();
        let source_binding_id = detail
            .evidence
            .first()
            .map(|event| event.identity.binding_id)
            .ok_or(ContainmentError::SourcePolicyUnavailable(case_id))?;
        let source_policy_snapshot = self
            .enforcer
            .credential_policy_snapshot(source_binding_id)
            .map_err(|error| ContainmentError::Enforcer(sanitize_failure(&error.to_string())))?;
        resolve_policy(detail, bindings, source_policy_snapshot)
            .ok_or(ContainmentError::SourcePolicyUnavailable(case_id))
    }
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "containment_adapter_tests.rs"]
mod adapter_tests;
