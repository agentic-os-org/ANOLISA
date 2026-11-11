//! Owner-bound runtime authorization and checked execution receipts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use asc_daemon_protocol::authorization::{
    AcceptedAuthorization, AuthorizationRequest, ClaimPhase, ClaimResponse, Decision,
    DecisionClaim, DecisionVerdict, ExecutionReceipt, ExecutionSpec, ReceiptAck, Validate,
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
    request: AuthorizationRequest,
    decision: Decision,
    accepted_at: Option<u64>,
    execution: Option<ExecutionSpec>,
    claimed: bool,
    run_id: Option<String>,
    reporting: bool,
    failed_receipt: Option<SecurityEvent>,
}

impl PendingDecision {
    fn accept(&mut self, claim: &DecisionClaim, now: u64) -> Result<(), AuthorizationError> {
        if self.accepted_at.is_some() || self.decision.expires_at <= now {
            return Err(AuthorizationError::Conflict);
        }
        let execution = claim
            .execution
            .as_ref()
            .ok_or(AuthorizationError::InvalidArgument)?;
        self.decision
            .validate_execution(execution)
            .map_err(|_| AuthorizationError::Conflict)?;
        self.execution = Some(execution.clone());
        self.run_id.clone_from(&claim.run_id);
        self.accepted_at = Some(now);
        Ok(())
    }
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
    /// Rejects invalid requests, exhausted decision capacity.
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
                    || item.accepted_at.is_some()
                    || item
                        .decision
                        .expires_at
                        .saturating_add(RECEIPT_GRACE_SECONDS)
                        >= now
            });
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
        pending.insert(
            decision.decision_id.clone(),
            PendingDecision {
                owner: peer.uid(),
                request: request.clone(),
                decision: decision.clone(),
                accepted_at: None,
                claimed: false,
                run_id: None,
                reporting: false,
                failed_receipt: None,
                execution: None,
            },
        );
        Ok(decision)
    }

    /// Transfers a fixed decision between a host hook and its eventual executor.
    ///
    /// # Errors
    /// Rejects foreign peers, expired acceptance, duplicate execution and invalid transitions.
    pub fn claim(
        &self,
        peer: PeerCredentials,
        claim: &DecisionClaim,
        now: u64,
    ) -> Result<ClaimResponse, AuthorizationError> {
        if claim
            .run_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128)
            || claim.decision_id.len() > 256
            || (claim.phase != ClaimPhase::Accept && claim.execution.is_some())
            || (!matches!(claim.phase, ClaimPhase::Accept | ClaimPhase::FinishRun)
                && claim.run_id.is_some())
            || (claim.phase == ClaimPhase::FinishRun) != claim.decision_id.is_empty()
        {
            return Err(AuthorizationError::InvalidArgument);
        }
        let response = ClaimResponse {
            decision_id: claim.decision_id.clone(),
            authorization: None,
        };
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| AuthorizationError::Internal)?;
        let id = if claim.phase == ClaimPhase::FinishRun {
            let run = claim
                .run_id
                .as_deref()
                .ok_or(AuthorizationError::InvalidArgument)?;
            let Some((id, _)) = pending.iter().find(|(_, item)| {
                item.owner == peer.uid() && item.run_id.as_deref() == Some(run) && !item.claimed
            }) else {
                return Ok(response);
            };
            id.clone()
        } else {
            claim.decision_id.clone()
        };
        let finish = matches!(claim.phase, ClaimPhase::Abandon | ClaimPhase::FinishRun);
        let Some(item) = pending.get_mut(&id) else {
            return if finish {
                Ok(response)
            } else {
                Err(AuthorizationError::Conflict)
            };
        };
        if item.owner != peer.uid() {
            return Err(AuthorizationError::Forbidden);
        }
        if finish && item.reporting {
            return Ok(response);
        }
        if item.reporting
            || (!finish && item.failed_receipt.is_some())
            || !matches!(
                item.decision.verdict,
                DecisionVerdict::Allow | DecisionVerdict::Modify | DecisionVerdict::StepUp
            )
        {
            return Err(AuthorizationError::Conflict);
        }
        if claim.phase == ClaimPhase::Accept {
            item.accept(claim, now)?;
            return Ok(response);
        }
        if finish {
            if item.accepted_at.is_none() {
                return Err(AuthorizationError::Conflict);
            }
            let event = if let Some(event) = item.failed_receipt.take() {
                event
            } else {
                host_finished_event(peer, &item.decision, now, item.claimed)?
            };
            item.execution = None;
            item.reporting = true;
            drop(pending);
            self.record_terminal(&id, event)?;
            return Ok(response);
        }
        if item.claimed {
            return Err(AuthorizationError::Conflict);
        }
        let accepted_at = item.accepted_at.ok_or(AuthorizationError::Conflict)?;
        let execution = item.execution.take().ok_or(AuthorizationError::Conflict)?;
        item.claimed = true;
        let mut response = response;
        response.authorization = Some(AcceptedAuthorization {
            execution,
            request: item.request.clone(),
            decision: item.decision.clone(),
            accepted_at,
        });
        Ok(response)
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
        if item.reporting {
            return Err(AuthorizationError::Conflict);
        }
        validate_receipt(&item.decision, receipt)?;
        let event = receipt_event(peer, receipt, &item.decision)?;
        if item
            .failed_receipt
            .as_ref()
            .is_some_and(|saved| saved.details != event.details)
        {
            return Err(AuthorizationError::Conflict);
        }
        let event = item.failed_receipt.take().unwrap_or(event);
        item.reporting = true;
        drop(pending);
        self.record_terminal(&receipt.decision_id, event)?;
        Ok(ReceiptAck {
            decision_id: receipt.decision_id.clone(),
            recorded: true,
        })
    }

    fn record_terminal(&self, id: &str, event: SecurityEvent) -> Result<(), AuthorizationError> {
        let result = self.sink.record(&event);
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| AuthorizationError::Internal)?;
        if result.is_ok() {
            pending.remove(id);
        } else if let Some(item) = pending.get_mut(id) {
            item.failed_receipt = Some(event);
            item.reporting = false;
        }
        result
    }
}

fn host_finished_event(
    peer: PeerCredentials,
    decision: &Decision,
    now: u64,
    claimed: bool,
) -> Result<SecurityEvent, AuthorizationError> {
    let receipt = ExecutionReceipt {
        decision_id: decision.decision_id.clone(),
        request_id: decision.request_id.clone(),
        execution_id: decision.execution_id.clone(),
        approval: None,
        result: asc_daemon_protocol::authorization::ExecutionResult {
            error: Some("host did not execute the authorized invocation".into()),
            ..Default::default()
        },
        recorded_at: now,
    };
    let mut event = receipt_event(peer, &receipt, decision)?;
    event.details.insert("host_finished".into(), true.into());
    if claimed {
        // The wrapper may have been killed after producing side effects.
        event
            .details
            .insert("result".into(), serde_json::Value::Null);
        event
            .details
            .insert("execution_outcome".into(), "unknown".into());
    }
    Ok(event)
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
    use asc_daemon_protocol::authorization::ExecutionResult;
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
                approval_requirements: vec![],
                warnings: vec!["test warning".into()],
                require_files: vec![],
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
            "principal":{"agent_type":"codex"},
            "launch":{"program":"python3","resolved_program":"/usr/bin/python3",
                "args":["agent.py"],"cwd":"/work"},
            "requested_permissions":{"version":1,"filesystem":{"default":"deny","rules":[]},
                "execution":{"deny_paths":[]},"network":{"mode":"off"}}
        }))
        .unwrap()
    }
    fn execution(decision: &Decision) -> ExecutionSpec {
        let mut launch = serde_json::to_value(&decision.launch).unwrap();
        launch["env"] = json!({"BOUND": "original"});
        ExecutionSpec {
            version: 1,
            launch: serde_json::from_value(launch).unwrap(),
            permissions: decision.effective_permissions.clone().unwrap(),
            path_facts: vec![],
            mount_aliases: vec![],
        }
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
    fn host_handoff_is_owner_bound_single_use_and_keeps_accepted_policy() {
        let (service, evaluator, _) = setup(DecisionVerdict::StepUp);
        let request = request();
        let decision = service.evaluate(peer(1000), &request, 100).unwrap();
        let mut claim = DecisionClaim {
            decision_id: decision.decision_id.clone(),
            run_id: None,
            phase: ClaimPhase::Execute,
            execution: None,
        };
        assert!(service.claim(peer(1000), &claim, 101).is_err());
        claim.phase = ClaimPhase::Accept;
        claim.execution = Some(execution(&decision));
        claim
            .execution
            .as_mut()
            .unwrap()
            .launch
            .args
            .push("tampered".into());
        assert!(service.claim(peer(1000), &claim, 101).is_err());
        claim.execution = Some(execution(&decision));
        assert!(matches!(
            service.claim(peer(1001), &claim, 101),
            Err(AuthorizationError::Forbidden)
        ));
        assert!(
            service
                .claim(peer(1000), &claim, 101)
                .unwrap()
                .authorization
                .is_none()
        );
        assert!(service.claim(peer(1000), &claim, 102).is_err());
        // New requests and cleanup cannot invalidate an invocation already waiting for approval.
        service.evaluate(peer(1000), &request, 200_000).unwrap();
        claim.execution = None;
        claim.phase = ClaimPhase::Execute;
        let accepted = service
            .claim(peer(1000), &claim, 200_001)
            .unwrap()
            .authorization
            .unwrap();
        assert_eq!(accepted.execution, execution(&decision));
        assert_eq!(accepted.request, request);
        assert_eq!(accepted.decision, decision);
        assert_eq!(accepted.accepted_at, 101);
        assert!(service.claim(peer(1000), &claim, 200_002).is_err());
        claim.execution = None;
        let mut report = receipt(&decision);
        report.approval = Some(true);
        claim.phase = ClaimPhase::Abandon;
        service.report(peer(1000), &report).unwrap();
        assert!(
            service
                .claim(peer(1000), &claim, 200_004)
                .unwrap()
                .authorization
                .is_none()
        );
        assert_eq!(evaluator.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn late_acceptance_and_nonexecutable_handoffs_are_rejected() {
        for verdict in [
            DecisionVerdict::Allow,
            DecisionVerdict::Deny,
            DecisionVerdict::Defer,
        ] {
            let (service, _, _) = setup(verdict);
            let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
            let claim = DecisionClaim {
                decision_id: decision.decision_id.clone(),
                run_id: None,
                phase: ClaimPhase::Accept,
                execution: Some(execution(&decision)),
            };
            assert!(service.claim(peer(1000), &claim, 161).is_err());
            if verdict != DecisionVerdict::Allow {
                assert!(service.claim(peer(1000), &claim, 101).is_err());
            }
        }
    }

    #[test]
    fn declined_host_handoff_can_only_be_reported_not_executed_again() {
        let (service, _, sink) = setup(DecisionVerdict::StepUp);
        let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
        let mut claim = DecisionClaim {
            decision_id: decision.decision_id.clone(),
            run_id: None,
            phase: ClaimPhase::Accept,
            execution: Some(execution(&decision)),
        };
        service.claim(peer(1000), &claim, 101).unwrap();
        claim.execution = None;
        claim.phase = ClaimPhase::Abandon;
        assert!(
            service
                .claim(peer(1000), &claim, 500)
                .unwrap()
                .authorization
                .is_none()
        );
        claim.execution = None;
        claim.phase = ClaimPhase::Execute;
        assert!(service.claim(peer(1000), &claim, 501).is_err());
        let mut report = receipt(&decision);
        report.approval = Some(false);
        report.result = ExecutionResult::default();
        assert!(service.report(peer(1000), &report).is_err());
        assert_eq!(sink.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn run_cleanup_keeps_other_approvals_and_claimed_execution() {
        let (service, _, sink) = setup(DecisionVerdict::StepUp);
        let accepted = |run: &str| {
            let decision = service.evaluate(peer(1000), &request(), 100).unwrap();
            service
                .claim(
                    peer(1000),
                    &DecisionClaim {
                        decision_id: decision.decision_id.clone(),
                        run_id: Some(run.into()),
                        phase: ClaimPhase::Accept,
                        execution: Some(execution(&decision)),
                    },
                    101,
                )
                .unwrap();
            decision.decision_id
        };
        let skipped = accepted("foreground");
        let waiting = accepted("waiting");
        let finish = DecisionClaim {
            decision_id: String::new(),
            phase: ClaimPhase::FinishRun,
            execution: None,
            run_id: Some("foreground".into()),
        };
        service.claim(peer(1001), &finish, 200_000).unwrap();
        assert!(sink.events.lock().unwrap().is_empty());
        service.claim(peer(1000), &finish, 200_000).unwrap();
        service.claim(peer(1000), &finish, 200_001).unwrap();
        let mut take = DecisionClaim {
            decision_id: skipped,
            phase: ClaimPhase::Execute,
            execution: None,
            run_id: None,
        };
        assert!(service.claim(peer(1000), &take, 200_002).is_err());
        take.decision_id = waiting;
        assert!(
            service
                .claim(peer(1000), &take, 200_002)
                .unwrap()
                .authorization
                .is_some()
        );
        service
            .claim(
                peer(1000),
                &DecisionClaim {
                    run_id: Some("waiting".into()),
                    ..finish
                },
                200_003,
            )
            .unwrap();
        assert_eq!(sink.events.lock().unwrap().len(), 1);
        take.phase = ClaimPhase::Abandon;
        service.claim(peer(1000), &take, 200_004).unwrap();
        service.claim(peer(1000), &take, 200_005).unwrap();
        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].details["result"]["started"], false);
        assert_eq!(events[1].details["execution_outcome"], "unknown");
        assert!(events[1].details["result"].is_null());
        assert!(service.pending.lock().unwrap().is_empty());
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
        assert_eq!(events[0].details["principal"]["agent_type"], "codex");
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
    fn host_finish_retries_the_original_executor_receipt_after_a_storage_failure() {
        let (service, _, sink) = setup(DecisionVerdict::Allow);
        let executor = peer(1000);
        let decision = service.evaluate(executor, &request(), 100).unwrap();
        let mut claim = DecisionClaim {
            decision_id: decision.decision_id.clone(),
            phase: ClaimPhase::Accept,
            execution: Some(execution(&decision)),
            run_id: None,
        };
        service.claim(executor, &claim, 101).unwrap();
        claim.phase = ClaimPhase::Execute;
        claim.execution = None;
        service.claim(executor, &claim, 102).unwrap();
        let mut report = receipt(&decision);
        sink.fails.store(true, Ordering::SeqCst);
        assert!(matches!(
            service.report(executor, &report),
            Err(AuthorizationError::Storage)
        ));
        report.result.exit_code = Some(1);
        assert!(matches!(
            service.report(executor, &report),
            Err(AuthorizationError::Conflict)
        ));
        sink.fails.store(false, Ordering::SeqCst);
        claim.phase = ClaimPhase::Abandon;
        service
            .claim(PeerCredentials::new(1000, 1000, 9999), &claim, 130)
            .unwrap();
        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pid, executor.pid());
        assert_eq!(events[0].details["result"]["exit_code"], 0);
        assert!(!events[0].details.contains_key("host_finished"));
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
