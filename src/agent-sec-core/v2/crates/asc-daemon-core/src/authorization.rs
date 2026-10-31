//! Owner-bound runtime authorization and checked execution receipts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use asc_policy_types::{
    AuthorizationRequest, Decision, DecisionVerdict, ExecutionReceipt, ReceiptAck, Validate,
};
use asc_security_events::{EventResult, SecurityEvent};

use crate::PeerCredentials;

const MAX_PENDING_DECISIONS: usize = 4096;
const MAX_PENDING_PER_UID: usize = 128;
// Receipts can arrive after the acceptance deadline because targets may run for hours.
const RECEIPT_GRACE_SECONDS: u64 = 86_400;

/// Process-owned PDP port; requests cannot supply policies.
pub trait AuthorizationEvaluator: Send + Sync {
    /// Evaluates against one trusted policy snapshot.
    ///
    /// # Errors
    /// Returns invalid-input, policy-loading or evaluation failures.
    fn evaluate(
        &self,
        request: &AuthorizationRequest,
        now: u64,
    ) -> Result<Decision, AuthorizationError>;
}

/// Receipt storage port independent of the concrete event destinations.
pub trait AuthorizationReceiptSink: Send + Sync {
    /// Writes the event to every required destination before returning success.
    ///
    /// # Errors
    /// Returns storage failure, including partial writes.
    fn record(&self, event: &SecurityEvent) -> Result<(), AuthorizationError>;
}

struct PendingDecision {
    owner: u32,
    decision: Decision,
    reporting: bool,
    reported: bool,
}

/// Coordinates PDP calls and correlates receipts with the authenticated caller.
pub struct AuthorizationService {
    evaluator: Arc<dyn AuthorizationEvaluator>,
    sink: Arc<dyn AuthorizationReceiptSink>,
    pending: Mutex<BTreeMap<String, PendingDecision>>,
}

impl AuthorizationService {
    /// Binds the trusted PDP and checked security-event writer.
    pub fn new(
        evaluator: Arc<dyn AuthorizationEvaluator>,
        sink: Arc<dyn AuthorizationReceiptSink>,
    ) -> Self {
        Self {
            evaluator,
            sink,
            pending: Mutex::new(BTreeMap::new()),
        }
    }

    /// Authorizes without launching a process or waiting for approval.
    ///
    /// # Errors
    /// Rejects invalid requests, foreign continuations and exhausted decision capacity.
    pub fn evaluate(
        &self,
        peer: PeerCredentials,
        request: &AuthorizationRequest,
        now: u64,
    ) -> Result<Decision, AuthorizationError> {
        request
            .validate()
            .map_err(|_| AuthorizationError::InvalidArgument)?;
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| AuthorizationError::Internal)?;
            pending.retain(|_, item| {
                item.reporting
                    || if item.decision.verdict == DecisionVerdict::Defer {
                        item.decision.expires_at > now
                    } else {
                        item.decision
                            .expires_at
                            .saturating_add(RECEIPT_GRACE_SECONDS)
                            >= now
                    }
            });
            if let Some(id) = &request.continuation {
                let previous = pending.get(id).ok_or(AuthorizationError::Conflict)?;
                if previous.owner != peer.uid() {
                    return Err(AuthorizationError::Forbidden);
                }
                if previous.decision.execution_id != request.execution_id
                    || previous.decision.principal != request.principal
                    || previous.decision.verdict != DecisionVerdict::Defer
                    || previous.decision.expires_at <= now
                {
                    return Err(AuthorizationError::Conflict);
                }
            }
            ensure_capacity(&pending, peer.uid())?;
        }
        let decision = self.evaluator.evaluate(request, now)?;
        if decision.request_id != request.request_id
            || decision.execution_id != request.execution_id
            || decision.principal != request.principal
            || decision.request_digest
                != request.digest().map_err(|_| AuthorizationError::Internal)?
            || decision.decision_id.is_empty()
            || decision.expires_at <= now
        {
            return Err(AuthorizationError::Internal);
        }
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| AuthorizationError::Internal)?;
        ensure_capacity(&pending, peer.uid())?;
        if pending.contains_key(&decision.decision_id) {
            return Err(AuthorizationError::Internal);
        }
        if let Some(previous) = &request.continuation
            && decision.continuation.as_ref() != Some(previous)
        {
            pending.remove(previous);
        }
        pending.insert(
            decision.decision_id.clone(),
            PendingDecision {
                owner: peer.uid(),
                decision: decision.clone(),
                reporting: false,
                reported: false,
            },
        );
        Ok(decision)
    }

    /// Records a bound receipt; successful acknowledgement does not mean the command succeeded.
    ///
    /// # Errors
    /// Rejects unknown, foreign, inconsistent or repeated receipts and checked storage failures.
    pub fn report(
        &self,
        peer: PeerCredentials,
        receipt: &ExecutionReceipt,
    ) -> Result<ReceiptAck, AuthorizationError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| AuthorizationError::Internal)?;
        let item = pending
            .get_mut(&receipt.decision_id)
            .ok_or(AuthorizationError::Conflict)?;
        if item.owner != peer.uid() {
            return Err(AuthorizationError::Forbidden);
        }
        if item.reporting || item.reported {
            return Err(AuthorizationError::Conflict);
        }
        validate_receipt(&item.decision, receipt)?;
        let decision = item.decision.clone();
        let retains_continuation = decision.verdict == DecisionVerdict::Defer
            && decision.continuation.as_deref() == Some(decision.decision_id.as_str());
        item.reporting = true;
        drop(pending);

        let result =
            receipt_event(peer, receipt, &decision).and_then(|event| self.sink.record(&event));
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| AuthorizationError::Internal)?;
        if result.is_ok() && !retains_continuation {
            pending.remove(&receipt.decision_id);
        } else if let Some(item) = pending.get_mut(&receipt.decision_id) {
            item.reporting = false;
            item.reported = result.is_ok();
        }
        result?;
        Ok(ReceiptAck {
            decision_id: receipt.decision_id.clone(),
            recorded: true,
        })
    }
}

fn ensure_capacity(
    pending: &BTreeMap<String, PendingDecision>,
    owner: u32,
) -> Result<(), AuthorizationError> {
    if pending.len() >= MAX_PENDING_DECISIONS
        || pending
            .values()
            .filter(|entry| entry.owner == owner)
            .count()
            >= MAX_PENDING_PER_UID
    {
        return Err(AuthorizationError::ResourceExhausted);
    }
    Ok(())
}

fn validate_receipt(
    decision: &Decision,
    receipt: &ExecutionReceipt,
) -> Result<(), AuthorizationError> {
    let result = &receipt.result;
    if receipt.request_id != decision.request_id
        || receipt.execution_id != decision.execution_id
        || (result.started && !result.installed)
        || (result.exit_code.is_some() && result.signal.is_some())
        || (!result.started && (result.exit_code.is_some() || result.signal.is_some()))
        || (result.started
            && matches!(
                decision.verdict,
                DecisionVerdict::Deny | DecisionVerdict::Defer
            ))
        || (result.started
            && decision.verdict == DecisionVerdict::StepUp
            && receipt.approval != Some(true))
        || (decision.verdict != DecisionVerdict::StepUp && receipt.approval.is_some())
    {
        return Err(AuthorizationError::InvalidArgument);
    }
    Ok(())
}

fn receipt_event(
    peer: PeerCredentials,
    receipt: &ExecutionReceipt,
    decision: &Decision,
) -> Result<SecurityEvent, AuthorizationError> {
    let serde_json::Value::Object(mut details) =
        serde_json::to_value(receipt).map_err(|_| AuthorizationError::Internal)?
    else {
        return Err(AuthorizationError::Internal);
    };
    details.insert(
        "policy_version".into(),
        decision.policy_version.clone().into(),
    );
    details.insert(
        "request_digest".into(),
        decision.request_digest.clone().into(),
    );
    details.insert(
        "principal".into(),
        serde_json::to_value(&decision.principal).map_err(|_| AuthorizationError::Internal)?,
    );
    details.insert(
        "verdict".into(),
        serde_json::to_value(decision.verdict).map_err(|_| AuthorizationError::Internal)?,
    );
    details.insert(
        "warnings".into(),
        serde_json::to_value(&decision.warnings).map_err(|_| AuthorizationError::Internal)?,
    );
    let mut event = SecurityEvent::new("authorization_receipt", "sandbox", details);
    event.uid = peer.uid();
    event.pid = peer.pid();
    event.run_id = Some(receipt.execution_id.clone());
    event.result = if receipt.result.started
        && receipt.result.exit_code == Some(0)
        && receipt.result.cancel_signal.is_none()
        && receipt.result.error.is_none()
    {
        EventResult::Succeeded
    } else {
        EventResult::Failed
    };
    Ok(event)
}

/// Safe runtime authorization failures for protocol projection.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizationError {
    /// Request or receipt violates the shared contract.
    #[error("invalid authorization request or receipt")]
    InvalidArgument,
    /// The authenticated caller does not own the referenced decision.
    #[error("authorization decision belongs to another caller")]
    Forbidden,
    /// Unknown, expired or already reported decision.
    #[error("authorization decision is unavailable or already reported")]
    Conflict,
    /// A trusted valid policy snapshot is not currently available.
    #[error("authorization policy is unavailable")]
    Unavailable,
    /// Bounded pending receipt capacity is exhausted.
    #[error("authorization decision capacity is exhausted")]
    ResourceExhausted,
    /// Required receipt storage failed; partial writes are possible.
    #[error("failed to record authorization receipt; partial write is possible")]
    Storage,
    /// A process-owned dependency violated its contract.
    #[error("authorization could not be completed")]
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_policy_types::ExecutionResult;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Evaluator {
        calls: AtomicUsize,
        verdict: DecisionVerdict,
    }
    impl AuthorizationEvaluator for Evaluator {
        fn evaluate(
            &self,
            request: &AuthorizationRequest,
            now: u64,
        ) -> Result<Decision, AuthorizationError> {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Decision {
                decision_id: format!("decision-{index}"),
                request_id: request.request_id.clone(),
                execution_id: request.execution_id.clone(),
                request_digest: request.digest().unwrap(),
                policy_version: "publication-1".into(),
                expires_at: now + 60,
                principal: request.principal.clone(),
                launch: request.launch.clone(),
                verdict: self.verdict,
                effective_permissions: Some(request.requested_permissions.clone()),
                reason: "test".into(),
                warnings: vec!["test warning".into()],
                candidate_launch: None,
                missing_paths: vec![],
                continuation: None,
            })
        }
    }
    #[derive(Default)]
    struct Sink {
        fails: AtomicBool,
        events: Mutex<Vec<SecurityEvent>>,
    }
    impl AuthorizationReceiptSink for Sink {
        fn record(&self, event: &SecurityEvent) -> Result<(), AuthorizationError> {
            if self.fails.load(Ordering::SeqCst) {
                return Err(AuthorizationError::Storage);
            }
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }
    fn setup(verdict: DecisionVerdict) -> (AuthorizationService, Arc<Evaluator>, Arc<Sink>) {
        let evaluator = Arc::new(Evaluator {
            calls: AtomicUsize::new(0),
            verdict,
        });
        let sink = Arc::new(Sink::default());
        (
            AuthorizationService::new(evaluator.clone(), sink.clone()),
            evaluator,
            sink,
        )
    }
    fn request() -> AuthorizationRequest {
        serde_json::from_value(json!({
            "type":"os", "request_id":"request-1", "execution_id":"execution-1",
            "principal":{"agent_id":"agent-1","agent_type":"codex"},
            "launch":{"program":"python3","resolved_program":"/usr/bin/python3",
                "args":["agent.py"],"cwd":"/work","env":{}},
            "requested_permissions":{"version":1,"filesystem":{"default":"deny","rules":[]},
                "execution":{"deny_paths":[]},"network":{"mode":"off"}},
            "path_facts":[], "capabilities":{"strict_filesystem":true,"noexec":true,"network_off":true,"ask":true}
        })).unwrap()
    }
    fn receipt(decision: &Decision) -> ExecutionReceipt {
        ExecutionReceipt {
            decision_id: decision.decision_id.clone(),
            request_id: decision.request_id.clone(),
            execution_id: decision.execution_id.clone(),
            approval: None,
            recorded_at: 120,
            result: ExecutionResult {
                installed: true,
                started: true,
                exit_code: Some(0),
                signal: None,
                cancel_signal: None,
                error: None,
            },
        }
    }
    fn peer(uid: u32) -> PeerCredentials {
        PeerCredentials::new(uid, uid, 1234)
    }

    #[test]
    fn receipt_binds_authenticated_owner_and_records_once() {
        let (service, evaluator, sink) = setup(DecisionVerdict::Allow);
        let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
        let mut receipt = receipt(&decision);
        assert!(matches!(
            service.report(peer(1001), &receipt),
            Err(AuthorizationError::Forbidden)
        ));
        receipt.execution_id = "another-execution".into();
        assert!(matches!(
            service.report(peer(1000), &receipt),
            Err(AuthorizationError::InvalidArgument)
        ));
        receipt.execution_id = decision.execution_id.clone();
        let ack = service.report(peer(1000), &receipt).unwrap();
        assert!(ack.recorded);
        assert_eq!(ack.decision_id, decision.decision_id);
        assert!(matches!(
            service.report(peer(1000), &receipt),
            Err(AuthorizationError::Conflict)
        ));
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 1);
        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid, 1000);
        assert_eq!(events[0].pid, 1234);
        assert_eq!(events[0].run_id.as_deref(), Some("execution-1"));
        assert_eq!(events[0].details["decision_id"], decision.decision_id);
        assert_eq!(events[0].details["policy_version"], "publication-1");
        assert_eq!(events[0].details["principal"]["agent_id"], "agent-1");
        assert_eq!(events[0].details["warnings"], json!(["test warning"]));
    }

    #[test]
    fn failed_storage_cannot_ack_or_rerun_authorization() {
        let (service, evaluator, sink) = setup(DecisionVerdict::Allow);
        let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
        let receipt = receipt(&decision);
        sink.fails.store(true, Ordering::SeqCst);
        assert!(matches!(
            service.report(peer(1000), &receipt),
            Err(AuthorizationError::Storage)
        ));
        assert!(sink.events.lock().unwrap().is_empty());
        sink.fails.store(false, Ordering::SeqCst);
        assert!(service.report(peer(1000), &receipt).unwrap().recorded);
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn step_up_requires_approval_and_denied_targets_cannot_start() {
        let (service, _, _) = setup(DecisionVerdict::StepUp);
        let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
        let mut report = receipt(&decision);
        assert!(matches!(
            service.report(peer(1000), &report),
            Err(AuthorizationError::InvalidArgument)
        ));
        report.approval = Some(true);
        assert!(service.report(peer(1000), &report).is_ok());
        for verdict in [DecisionVerdict::Deny, DecisionVerdict::Defer] {
            let (service, _, _) = setup(verdict);
            let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
            let mut report = receipt(&decision);
            assert!(matches!(
                service.report(peer(1000), &report),
                Err(AuthorizationError::InvalidArgument)
            ));
            report.result = ExecutionResult::default();
            assert!(service.report(peer(1000), &report).unwrap().recorded);
        }
    }

    #[test]
    fn continuation_is_owned_even_after_defer_receipt() {
        let (service, evaluator, _) = setup(DecisionVerdict::Defer);
        let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
        service
            .pending
            .lock()
            .unwrap()
            .get_mut(&decision.decision_id)
            .unwrap()
            .decision
            .continuation = Some(decision.decision_id.clone());
        let mut report = receipt(&decision);
        report.result = ExecutionResult::default();
        service.report(peer(1000), &report).unwrap();
        let mut next = request();
        next.request_id = "request-2".into();
        next.continuation = Some(decision.decision_id);
        assert!(matches!(
            service.evaluate(peer(1001), &next, 110),
            Err(AuthorizationError::Forbidden)
        ));
        next.execution_id = "wrong".into();
        assert!(matches!(
            service.evaluate(peer(1000), &next, 110),
            Err(AuthorizationError::Conflict)
        ));
        next.execution_id = "execution-1".into();
        service.evaluate(peer(1000), &next, 110).unwrap();
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn completed_fact_deferrals_do_not_exhaust_decision_capacity() {
        let (service, _, _) = setup(DecisionVerdict::Defer);
        for _ in 0..MAX_PENDING_PER_UID + 2 {
            let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
            let mut report = receipt(&decision);
            report.result = ExecutionResult::default();
            service.report(peer(1000), &report).unwrap();
        }
        assert!(service.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn decision_capacity_is_bounded_per_uid_and_expired_entries_are_reclaimed() {
        let (service, evaluator, _) = setup(DecisionVerdict::Allow);
        for _ in 0..MAX_PENDING_PER_UID {
            service.evaluate(peer(1000), &request(), 100).unwrap();
        }
        assert!(matches!(
            service.evaluate(peer(1000), &request(), 100),
            Err(AuthorizationError::ResourceExhausted)
        ));
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), MAX_PENDING_PER_UID);
        service.evaluate(peer(1001), &request(), 100).unwrap();
        service
            .evaluate(peer(1000), &request(), 200 + RECEIPT_GRACE_SECONDS)
            .unwrap();
    }
}
