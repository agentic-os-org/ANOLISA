//! Durable permit consumption around one COSH-owned execution target.

use cosh_gateway_contracts::{
    capability::RuntimeExecutionFence,
    common::{BoundedText, Digest, TargetRef},
    ids::ExecutionId,
    runtime::BrokeredOperationResult,
};
use thiserror::Error;

use crate::storage::{
    ExecutionClaim, ExecutionCompletion, ExecutionRecord, LedgerCommand, LedgerOutcome,
    SecurityAuditProof, SqliteTaskStore, StoreError,
};

/// Trusted operation whose complete bindings were canonicalized before policy evaluation.
pub trait BoundExecutionOperation {
    /// Returns the exact target bound into the capability request and permit.
    fn target(&self) -> &TargetRef;

    /// Returns the immutable resolved identity of the exact target.
    fn target_identity_digest(&self) -> &Digest;

    /// Returns the exact Runtime and renewable Run-lease fence.
    fn runtime_fence(&self) -> &RuntimeExecutionFence;

    /// Returns the digest of the complete canonical operation.
    fn operation_digest(&self) -> &Digest;

    /// Returns the digest of the complete Runtime input admitted by policy.
    fn input_digest(&self) -> &Digest;
}

/// Conclusive or indeterminate result returned by a COSH-owned execution target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionTargetOutcome {
    /// The target knows whether the side effect succeeded and provides durable evidence.
    Conclusive {
        /// Whether the governed operation succeeded.
        succeeded: bool,
        /// Digest of the complete target receipt.
        receipt_digest: Digest,
        /// Optional redacted detail suitable for audit and local presentation.
        safe_detail: Option<BoundedText>,
        /// Typed result required for every successful COSH-brokered operation.
        typed_result: Option<BrokeredOperationResult>,
    },
    /// The target may have started a side effect but cannot prove its final result.
    Unknown {
        /// Optional redacted detail explaining why the outcome is indeterminate.
        safe_detail: Option<BoundedText>,
    },
}

/// Side-effect boundary invoked only after a permit is consumed durably.
pub trait ExecutionTarget<O: BoundExecutionOperation> {
    /// Executes one already-admitted operation.
    ///
    /// Implementations must return [`ExecutionTargetOutcome::Unknown`] when a
    /// transport or process failure makes the external result ambiguous. They
    /// must not retry an operation under the same single-use permit.
    fn execute(&mut self, operation: &O) -> ExecutionTargetOutcome;
}

/// Durable security audit boundary required before an external effect may start.
pub trait SecurityAuditGate<O: BoundExecutionOperation> {
    /// Persists a security-boundary start record and returns its exact proof.
    ///
    /// Returning proves the underlying writer completed its required durability
    /// barrier. Implementations must return an error when that cannot be proven.
    fn persist_start(
        &mut self,
        execution: &ExecutionRecord,
        operation: &O,
    ) -> Result<SecurityAuditProof, SecurityAuditError>;
}

/// Fail-closed audit failure before the external target receives control.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("security audit durability could not be proven")]
pub struct SecurityAuditError;

/// Durable result of one conclusive COSH-brokered execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedExecutionResult {
    /// Terminal execution ledger row.
    pub execution: ExecutionRecord,
    /// Conclusive result reported by the target.
    pub succeeded: bool,
    /// Typed result committed atomically with a successful receipt.
    pub typed_result: Option<BrokeredOperationResult>,
}

/// Command factory stage whose timestamp or canonical digest could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionCommandBuildStage {
    /// Post-audit command that admits the external effect boundary.
    Start,
    /// Post-audit-failure or post-target durable conclusion command.
    Terminal,
}

/// Failures around admission, execution, and receipt persistence.
#[derive(Debug, Error)]
pub enum GovernedExecutionError {
    /// Prepared operation differs from the exact permit claim.
    #[error("prepared operation does not match the execution claim")]
    BindingMismatch,
    /// A post-boundary clock or canonical command digest could not be produced.
    #[error("execution {execution_id} {stage:?} command construction failed: {message:?}")]
    CommandBuild {
        /// Execution whose durable transition could not be constructed.
        execution_id: ExecutionId,
        /// Boundary after which construction was attempted.
        stage: ExecutionCommandBuildStage,
        /// Bounded diagnostic excluding raw operation input.
        message: BoundedText,
    },
    /// Required security audit proof could not be persisted durably.
    #[error(transparent)]
    SecurityAudit(#[from] SecurityAuditError),
    /// Audit failed before the effect boundary and the durable claim was concluded safely.
    #[error("execution {execution_id} was not started because its security audit failed")]
    SecurityAuditKnownNoEffect {
        /// Execution durably proven not to have invoked its target.
        execution_id: ExecutionId,
        /// Original audit durability failure.
        source: SecurityAuditError,
    },
    /// Audit failed before effect, but its known-no-effect conclusion was not persisted.
    #[error(
        "execution {execution_id} did not start, but known-no-effect persistence failed: {source}"
    )]
    KnownNoEffectPersistenceFailed {
        /// Execution whose claimed state still requires recovery.
        execution_id: ExecutionId,
        /// Storage failure while concluding the claim.
        source: StoreError,
    },
    /// Permit admission or another pre-execution ledger mutation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A side effect may have run but the target cannot prove its result.
    #[error("execution {execution_id} has an unknown target outcome")]
    OutcomeUnknown {
        /// Execution whose one-shot authority must never be retried.
        execution_id: ExecutionId,
        /// Redacted target diagnostic.
        safe_detail: Option<BoundedText>,
    },
    /// A conclusive target result could not be committed durably.
    #[error("execution {execution_id} completed but its receipt could not be committed: {source}")]
    CompletionUnknown {
        /// Execution whose external result must not be retried automatically.
        execution_id: ExecutionId,
        /// Storage failure after the side effect returned.
        source: StoreError,
    },
}

/// Coordinates exact permit consumption and conclusive execution receipts.
pub struct GovernedExecutionCoordinator<'a> {
    store: &'a mut SqliteTaskStore,
}

impl<'a> GovernedExecutionCoordinator<'a> {
    /// Creates a coordinator around the authoritative Gateway ledger.
    pub fn new(store: &'a mut SqliteTaskStore) -> Self {
        Self { store }
    }

    /// Consumes one permit, invokes the target, and records its durable outcome.
    ///
    /// The start command is built only after the audit durability barrier. The
    /// terminal command factory is invoked after audit failure or target return,
    /// so both timestamps reflect the real durability boundary. Permit
    /// consumption and the `started` transition commit before the target
    /// receives control. Unknown results are immediately marked uncertain and
    /// never become retryable.
    ///
    /// # Errors
    ///
    /// Returns before execution when bindings, command construction, or ledger
    /// admission fail. Audit failure returns `SecurityAuditKnownNoEffect` only
    /// after the claim is durably concluded. Returns
    /// [`GovernedExecutionError::OutcomeUnknown`] when the target cannot prove
    /// the result, and
    /// [`GovernedExecutionError::CompletionUnknown`] when the result exists but
    /// its receipt could not be committed.
    pub fn execute<O, T, A, S, R>(
        &mut self,
        claim_command: &LedgerCommand,
        start_command: S,
        terminal_command: R,
        claim: &ExecutionClaim,
        operation: &O,
        target: &mut T,
        audit: &mut A,
    ) -> Result<GovernedExecutionResult, GovernedExecutionError>
    where
        O: BoundExecutionOperation,
        T: ExecutionTarget<O>,
        A: SecurityAuditGate<O>,
        S: FnOnce(&SecurityAuditProof) -> Result<LedgerCommand, GovernedExecutionError>,
        R: FnOnce() -> Result<LedgerCommand, GovernedExecutionError>,
    {
        if claim.target != *operation.target()
            || claim.target_identity_digest != *operation.target_identity_digest()
            || claim.runtime_fence != *operation.runtime_fence()
            || claim.operation_digest != *operation.operation_digest()
            || claim.input_digest != *operation.input_digest()
        {
            return Err(GovernedExecutionError::BindingMismatch);
        }

        let claimed = match self.store.claim_execution(claim_command, claim)? {
            LedgerOutcome::Applied(started) => started,
            LedgerOutcome::Replayed(_) => {
                // The original caller may already have crossed the external
                // side-effect boundary. Replaying it would violate the
                // single-use permit even though the ledger command is
                // idempotent.
                return Err(GovernedExecutionError::OutcomeUnknown {
                    execution_id: claim.execution_id.clone(),
                    safe_detail: None,
                });
            }
        };
        let proof = match audit.persist_start(&claimed, operation) {
            Ok(proof) => proof,
            Err(source) => {
                let failure_command = terminal_command()?;
                let safe_detail =
                    BoundedText::new("Security audit durability failed before the external effect")
                        .unwrap_or_else(|_| unreachable!("static audit failure detail is bounded"));
                self.store
                    .mark_claimed_execution_known_no_effect(
                        &failure_command,
                        &claim.execution_id,
                        claimed.revision,
                        &safe_detail,
                        &claim.lease,
                    )
                    .map_err(
                        |source| GovernedExecutionError::KnownNoEffectPersistenceFailed {
                            execution_id: claim.execution_id.clone(),
                            source,
                        },
                    )?;
                return Err(GovernedExecutionError::SecurityAuditKnownNoEffect {
                    execution_id: claim.execution_id.clone(),
                    source,
                });
            }
        };
        let start_command = match start_command(&proof) {
            Ok(command) => command,
            Err(build_error) => {
                let failure_command = terminal_command()?;
                let safe_detail = BoundedText::new(
                    "Execution command construction failed before the external effect",
                )
                .unwrap_or_else(|_| unreachable!("static command failure detail is bounded"));
                self.store
                    .mark_claimed_execution_known_no_effect(
                        &failure_command,
                        &claim.execution_id,
                        claimed.revision,
                        &safe_detail,
                        &claim.lease,
                    )
                    .map_err(
                        |source| GovernedExecutionError::KnownNoEffectPersistenceFailed {
                            execution_id: claim.execution_id.clone(),
                            source,
                        },
                    )?;
                return Err(build_error);
            }
        };
        let started = match self.store.start_claimed_execution(
            &start_command,
            &claim.execution_id,
            claimed.revision,
            &proof,
        )? {
            LedgerOutcome::Applied(started) => started,
            LedgerOutcome::Replayed(_) => {
                return Err(GovernedExecutionError::OutcomeUnknown {
                    execution_id: claim.execution_id.clone(),
                    safe_detail: None,
                });
            }
        };
        match target.execute(operation) {
            ExecutionTargetOutcome::Conclusive {
                succeeded,
                receipt_digest,
                safe_detail,
                typed_result,
            } => {
                let completion_command = terminal_command()?;
                let completion = ExecutionCompletion {
                    execution_id: claim.execution_id.clone(),
                    expected_revision: started.revision,
                    succeeded,
                    receipt_digest,
                    safe_detail,
                    typed_result: typed_result.clone(),
                };
                let execution = self
                    .store
                    .complete_execution(&completion_command, &completion)
                    .map(ledger_value)
                    .map_err(|source| GovernedExecutionError::CompletionUnknown {
                        execution_id: claim.execution_id.clone(),
                        source,
                    })?;
                Ok(GovernedExecutionResult {
                    execution,
                    succeeded,
                    typed_result,
                })
            }
            ExecutionTargetOutcome::Unknown { safe_detail } => {
                let uncertainty_command = terminal_command()?;
                let detail = safe_detail.clone().unwrap_or_else(|| {
                    BoundedText::new("Target result is indeterminate")
                        .unwrap_or_else(|_| unreachable!("static uncertainty detail is bounded"))
                });
                self.store
                    .mark_execution_uncertain(
                        &uncertainty_command,
                        &claim.execution_id,
                        started.revision,
                        &detail,
                        &claim.lease,
                    )
                    .map_err(|source| GovernedExecutionError::CompletionUnknown {
                        execution_id: claim.execution_id.clone(),
                        source,
                    })?;
                Err(GovernedExecutionError::OutcomeUnknown {
                    execution_id: claim.execution_id.clone(),
                    safe_detail,
                })
            }
        }
    }
}

fn ledger_value<T>(outcome: LedgerOutcome<T>) -> T {
    match outcome {
        LedgerOutcome::Applied(value) | LedgerOutcome::Replayed(value) => value,
    }
}

#[cfg(test)]
mod tests;
