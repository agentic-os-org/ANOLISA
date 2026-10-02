
/// Trusted input for admitting one Runtime-originated brokered operation.
pub struct BrokeredApprovalContext<'a> {
    /// Active authenticated Run that owns the callback.
    pub scheduled: &'a ScheduledRun,
    /// Exact Runtime callback identity.
    pub brokered: &'a BrokeredExecutionRef,
    /// Gateway-normalized capability request.
    pub request: &'a CapabilityRequest,
    /// Closed typed operation proposed for governed execution.
    pub operation: &'a BrokeredOperation,
    /// Redacted presentation supplied to policy.
    pub summary: &'a ToolSummary,
    /// Runtime and lease generation fence captured at admission.
    pub runtime_fence: &'a RuntimeExecutionFence,
    /// Admission timestamp.
    pub now_ms: u64,
}

/// Policy-produced approval plan with a trusted immutable target identity.
pub struct BrokeredApprovalPlan {
    /// Pending approval to persist before acknowledging the Runtime.
    pub approval: ApprovalRequest,
    /// Digest produced by a trusted target resolver, never by the scheduler.
    pub target_identity_digest: Digest,
    /// Provider-owned admission binding required to reconcile a started effect.
    ///
    /// The value is opaque to Gateway storage and is never exposed to the
    /// Runtime. Concrete drivers must version and validate their own encoding.
    pub provider_binding: Option<BoundedOpaque>,
}

/// Trusted input for resolving and optionally executing one brokered request.
pub struct BrokeredResolutionContext<'a> {
    /// Exact durable approval being resolved.
    pub approval: &'a ApprovalRecord,
    /// Complete durable request and target binding.
    pub request: &'a BrokeredRequestRecord,
    /// Exact live Runtime callback identity.
    pub brokered: &'a BrokeredExecutionRef,
    /// Current exact Run-lease claim; renewals may advance its revision only.
    pub lease: &'a LeaseClaim,
    /// Caller-stable key from the authenticated approval command.
    pub idempotency_key: &'a IdempotencyKey,
    /// Explicit actor decision.
    pub decision: ApprovalDecision,
    /// Resolution timestamp.
    pub now_ms: u64,
}

/// Exact durable inputs available when reconciling a started external effect.
pub struct BrokeredRecoveryContext<'a> {
    /// Started execution whose one-shot authority was already consumed.
    pub execution: &'a crate::storage::ExecutionRecord,
    /// Original request, operation, and provider-owned binding.
    pub request: &'a BrokeredRequestRecord,
}

/// Durable authority backing one terminal brokered Runtime result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokeredResolutionSource {
    /// A durable explicit approval denial proves that no target ran.
    ApprovalDenied {
        /// Explicitly denied approval authorizing the result.
        approval_id: ApprovalId,
    },
    /// A durable governed execution proves the typed target outcome.
    Execution {
        /// Governed execution authorizing the result.
        execution_id: ExecutionId,
    },
}

/// Result returned after the driver has durably resolved policy and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokeredResolution {
    /// Durable ledger fact that authorizes result dispatch.
    pub source: BrokeredResolutionSource,
    /// Complete typed payload delivered to the Runtime without a Permit.
    pub delivery: BrokeredExecutionDelivery,
}

/// Injected trusted boundary for brokered policy, target resolution, and execution.
pub trait BrokeredExecutionDriver: Send {
    /// Resolves immutable target identity and produces an approval plan.
    ///
    /// # Errors
    ///
    /// Returns a bounded failure when policy cannot safely admit the request.
    fn plan_approval(
        &mut self,
        context: BrokeredApprovalContext<'_>,
    ) -> Result<BrokeredApprovalPlan, ContractError>;

    /// Resolves policy and performs any approved target execution durably.
    ///
    /// The implementation owns policy re-evaluation, single-use Permit
    /// issuance, security audit, and typed target invocation. The scheduler
    /// owns only the subsequent non-replayable Runtime dispatch.
    ///
    /// # Errors
    ///
    /// Returns a bounded failure when the request cannot converge safely.
    fn resolve(
        &mut self,
        store: &mut SqliteTaskStore,
        context: BrokeredResolutionContext<'_>,
    ) -> Result<BrokeredResolution, ContractError>;

    /// Queries exact provider evidence after ownership of a started effect was lost.
    ///
    /// The default is deliberately indeterminate. Implementations may return a
    /// conclusive outcome only from evidence bound to the original durable
    /// request; this method must never replay the external operation.
    fn reconcile_started(
        &mut self,
        _context: BrokeredRecoveryContext<'_>,
    ) -> crate::capability::ExecutionTargetOutcome {
        crate::capability::ExecutionTargetOutcome::Unknown { safe_detail: None }
    }
}

pub(super) struct RejectingBrokeredExecutionDriver;

impl BrokeredExecutionDriver for RejectingBrokeredExecutionDriver {
    fn plan_approval(
        &mut self,
        _context: BrokeredApprovalContext<'_>,
    ) -> Result<BrokeredApprovalPlan, ContractError> {
        Err(runtime_handle_unsupported("brokered execution policy"))
    }

    fn resolve(
        &mut self,
        _store: &mut SqliteTaskStore,
        _context: BrokeredResolutionContext<'_>,
    ) -> Result<BrokeredResolution, ContractError> {
        Err(runtime_handle_unsupported("brokered execution policy"))
    }
}

#[derive(Debug, Clone)]
pub(super) struct PendingBrokered {
    pub(super) brokered: BrokeredExecutionRef,
    pub(super) approval: ApprovalRequest,
    pub(super) resolution: Option<BrokeredResolution>,
}
