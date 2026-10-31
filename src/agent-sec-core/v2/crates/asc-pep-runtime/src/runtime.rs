//! Optional upper PEP flow: prepare, authorize, approve, enforce and report.

use crate::{PepError, SandboxBackend, prepare_launch, prepare_path_facts};
use asc_policy_types::{
    AgentPrincipal, AuthorizationRequest, Decision, DecisionVerdict, ExecutionReceipt,
    ExecutionResult, ExecutionSpec, FileRight, LaunchSpec, NetworkMode, OsPermissionSet, PathFact,
    ReceiptAck, RequestType, RuleEffect, Validate,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Source of requested permissions; task/state-aware providers can capture their context.
pub trait PermissionProvider {
    /// Includes every needed dependency without changing the requested command.
    ///
    /// # Errors
    /// Returns an error when no valid permission request can be prepared.
    fn permissions(&self, launch: &LaunchSpec) -> Result<OsPermissionSet, PepError>;
}

/// Default provider for an explicit fixed permission set.
pub struct FixedPermissions(pub OsPermissionSet);
impl PermissionProvider for FixedPermissions {
    fn permissions(&self, _launch: &LaunchSpec) -> Result<OsPermissionSet, PepError> {
        self.0.validate()?;
        Ok(self.0.clone())
    }
}

/// Bounded authorization transport independent of the execution backend.
pub trait AuthorizationClient {
    /// Evaluates a prepared request without executing it.
    ///
    /// # Errors
    /// Returns transport, malformed response or service errors.
    fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError>;
    /// Records one receipt, without retrying execution.
    ///
    /// # Errors
    /// Returns transport or durable recording failures.
    fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError>;
}

/// Existing Unix socket client for the two authorization methods.
pub struct DaemonAuthorizationClient {
    /// System daemon socket.
    pub socket: PathBuf,
    /// Deadline for each independent RPC.
    pub timeout: Duration,
}

impl AuthorizationClient for DaemonAuthorizationClient {
    fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError> {
        match asc_daemon_client::authorization_evaluate(&self.socket, request, self.timeout)
            .map_err(|e| PepError::Authorization(e.to_string()))?
        {
            asc_daemon_protocol::DaemonResponse::Success(response) => Ok(response.result),
            asc_daemon_protocol::DaemonResponse::Error(response) => {
                Err(PepError::Authorization(response.error.message().into()))
            }
        }
    }
    fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError> {
        match asc_daemon_client::authorization_report(&self.socket, receipt, self.timeout)
            .map_err(|e| PepError::Authorization(e.to_string()))?
        {
            asc_daemon_protocol::DaemonResponse::Success(response) => Ok(response.result),
            asc_daemon_protocol::DaemonResponse::Error(response) => {
                Err(PepError::Authorization(response.error.message().into()))
            }
        }
    }
}

/// Explicit approval channel supplied by the embedding application or Hook.
pub trait ApprovalHandler {
    /// Approves only this immutable action and permission set.
    ///
    /// # Errors
    /// Timeout or unavailable confirmation must fail closed.
    fn ask(&mut self, decision: &Decision) -> Result<bool, PepError>;
}

/// Execution result survives a failed receipt RPC.
pub struct PepOutcome {
    /// Final decision that was implemented or refused.
    pub decision: Decision,
    /// Actual execution or non-execution outcome.
    pub result: ExecutionResult,
    /// Recording failure; callers must not rerun the command to repair it.
    pub receipt_error: Option<String>,
}

/// Upper PEP state; retain one instance across invocations to reject decision replay.
pub struct PepRuntime {
    backend: SandboxBackend,
    consumed: HashMap<String, u64>,
}

impl PepRuntime {
    /// Uses the same strict backend as the independent CLI.
    pub fn new(backend: SandboxBackend) -> Self {
        Self {
            backend,
            consumed: HashMap::new(),
        }
    }

    /// Prepares and authorizes a launch, handling at most eight fact/candidate rounds.
    ///
    /// # Errors
    /// Returns preparation or transport failures before a bound decision is available.
    /// Execution and receipt failures are preserved in `PepOutcome`.
    pub fn run(
        &mut self,
        principal: &AgentPrincipal,
        launch: LaunchSpec,
        provider: &dyn PermissionProvider,
        client: &dyn AuthorizationClient,
        mut approval: Option<&mut dyn ApprovalHandler>,
    ) -> Result<PepOutcome, PepError> {
        let mut launch = prepare_launch(
            &launch.program,
            launch.args,
            Path::new(&launch.cwd),
            launch.env,
        )?;
        let permissions = provider.permissions(&launch)?;
        let execution_id = uuid::Uuid::new_v4().to_string();
        let mut continuation = None;
        let mut extra_paths = Vec::new();
        for _ in 0..8 {
            let mut capabilities = self.backend.capabilities()?;
            capabilities.ask = approval.is_some();
            let request = AuthorizationRequest {
                request_type: RequestType::Os,
                request_id: uuid::Uuid::new_v4().to_string(),
                execution_id: execution_id.clone(),
                principal: principal.clone(),
                path_facts: prepare_path_facts(&launch, &permissions, &extra_paths)?,
                launch: launch.clone(),
                requested_permissions: permissions.clone(),
                capabilities,
                continuation: continuation.clone(),
            };
            request.validate()?;
            let decision = client.evaluate(&request)?;
            verify_binding(&request, &decision, now()?)?;
            if decision.verdict == DecisionVerdict::Defer {
                let waiting = ExecutionResult {
                    error: Some(decision.reason.clone()),
                    ..ExecutionResult::default()
                };
                // Each deferred decision gets one receipt; recording cannot grant execution.
                let receipt = receipt(&decision, None, waiting, now()?);
                let ack = client.report(&receipt)?;
                if !ack.recorded || ack.decision_id != decision.decision_id {
                    return Err(PepError::Protocol(
                        "invalid deferred receipt acknowledgement".into(),
                    ));
                }
                if decision.candidate_launch.is_none() && decision.missing_paths.is_empty() {
                    return Err(PepError::Authorization(
                        "DEFER supplied no actionable preparation".into(),
                    ));
                }
                if let Some(candidate) = decision.candidate_launch {
                    let original_cwd = candidate.cwd.clone();
                    launch = prepare_launch(
                        &candidate.program,
                        candidate.args,
                        Path::new(&candidate.cwd),
                        candidate.env,
                    )?;
                    extra_paths.clear();
                    extra_paths.push(original_cwd);
                }
                extra_paths.extend(decision.missing_paths);
                continuation = decision.continuation;
                continue;
            }
            return self.apply(&request, decision, client, approval.take());
        }
        Err(PepError::Authorization(
            "authorization preparation exceeded eight rounds".into(),
        ))
    }

    /// Accepts a pushed or pulled decision once, before its deadline, for this invocation.
    /// Approval and execution may finish after the acceptance deadline.
    ///
    /// # Errors
    /// Rejects mismatched, expired, repeated, or non-executable decisions.
    pub fn apply(
        &mut self,
        request: &AuthorizationRequest,
        decision: Decision,
        client: &dyn AuthorizationClient,
        approval: Option<&mut dyn ApprovalHandler>,
    ) -> Result<PepOutcome, PepError> {
        let time = now()?;
        verify_binding(request, &decision, time)?;
        self.consumed.retain(|_, expiry| *expiry > time);
        if self
            .consumed
            .insert(decision.decision_id.clone(), decision.expires_at)
            .is_some()
        {
            return Err(PepError::Authorization("decision already consumed".into()));
        }
        let mut approved = None;
        let execution = (|| {
            match decision.verdict {
                DecisionVerdict::Deny | DecisionVerdict::Defer => {
                    return Err(PepError::Authorization(decision.reason.clone()));
                }
                DecisionVerdict::StepUp => {
                    let channel = approval.ok_or_else(|| {
                        PepError::Authorization("STEP_UP requires an approval channel".into())
                    })?;
                    let accepted = channel.ask(&decision)?;
                    approved = Some(accepted);
                    if !accepted {
                        return Err(PepError::Authorization("approval declined".into()));
                    }
                }
                DecisionVerdict::Allow | DecisionVerdict::Modify => {}
            }
            // Resolve the final command locally without refreshing its authorized path facts.
            let prepared = prepare_launch(
                &decision.launch.program,
                decision.launch.args.clone(),
                Path::new(&decision.launch.cwd),
                decision.launch.env.clone(),
            )?;
            if prepared != decision.launch {
                return Err(PepError::Authorization(
                    "final launch no longer matches its authorized resolution".into(),
                ));
            }
            // Acceptance fixes this immutable request; approval and setup do not expire it.
            let permissions = decision.effective_permissions.clone().ok_or_else(|| {
                PepError::Protocol("executable decision has no effective permissions".into())
            })?;
            verify_ceiling(
                &permissions,
                &request.requested_permissions,
                &request.path_facts,
            )?;
            self.backend.execute_bound(&ExecutionSpec {
                version: 1,
                launch: decision.launch.clone(),
                permissions,
                path_facts: request.path_facts.clone(),
            })
        })();
        let result = execution.unwrap_or_else(|error| ExecutionResult {
            error: Some(error.to_string()),
            ..ExecutionResult::default()
        });
        let receipt = receipt(&decision, approved, result.clone(), now()?);
        let receipt_error = match client.report(&receipt) {
            Ok(ack) if ack.recorded && ack.decision_id == decision.decision_id => None,
            Ok(_) => Some("invalid receipt acknowledgement".into()),
            Err(error) => Some(error.to_string()),
        };
        Ok(PepOutcome {
            decision,
            result,
            receipt_error,
        })
    }
}

fn receipt(
    decision: &Decision,
    approval: Option<bool>,
    result: ExecutionResult,
    recorded_at: u64,
) -> ExecutionReceipt {
    ExecutionReceipt {
        decision_id: decision.decision_id.clone(),
        request_id: decision.request_id.clone(),
        execution_id: decision.execution_id.clone(),
        approval,
        result,
        recorded_at,
    }
}

fn now() -> Result<u64, PepError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| PepError::Authorization("clock predates Unix epoch".into()))
}

fn verify_binding(
    request: &AuthorizationRequest,
    decision: &Decision,
    time: u64,
) -> Result<(), PepError> {
    if decision.decision_id.is_empty()
        || decision.policy_version.is_empty()
        || decision.request_id != request.request_id
        || decision.execution_id != request.execution_id
        || decision.principal != request.principal
        || (decision.launch != request.launch
            && !matches!(
                decision.verdict,
                DecisionVerdict::Modify | DecisionVerdict::StepUp
            ))
        || decision.launch.env != request.launch.env
        || decision.request_digest != request.digest()?
        || decision.expires_at <= time
    {
        return Err(PepError::Authorization(
            "decision binding or expiry mismatch".into(),
        ));
    }
    Ok(())
}

fn verify_ceiling(
    effective: &OsPermissionSet,
    requested: &OsPermissionSet,
    facts: &[PathFact],
) -> Result<(), PepError> {
    effective.validate()?;
    if effective.network.mode == NetworkMode::Full && requested.network.mode != NetworkMode::Full {
        return Err(PepError::Authorization(
            "effective network exceeds request".into(),
        ));
    }
    for denied in &requested.execution.deny_paths {
        if !effective.execution.deny_paths.contains(denied) {
            return Err(PepError::Authorization(
                "effective permissions removed requested noexec".into(),
            ));
        }
    }
    let kinds: BTreeMap<_, _> = facts
        .iter()
        .map(|fact| (fact.canonical_path.as_str(), fact.is_directory))
        .collect();
    let mut paths = BTreeSet::new();
    for rule in requested
        .filesystem
        .rules
        .iter()
        .chain(&effective.filesystem.rules)
    {
        let canonical = std::fs::canonicalize(&rule.path)?;
        let canonical = canonical
            .to_str()
            .ok_or_else(|| PepError::Unsupported("non-UTF-8 permission path".into()))?
            .to_owned();
        let directory = kinds.get(canonical.as_str()).ok_or_else(|| {
            PepError::Authorization("permission path lacks an authorized identity".into())
        })?;
        paths.insert(canonical.clone());
        if *directory {
            paths.insert(format!(
                "{}/.asc-uncreated-child",
                canonical.trim_end_matches('/')
            ));
        }
    }
    for path in paths {
        for right in [FileRight::Read, FileRight::Write] {
            if granted(effective, &path, right, &kinds)?
                && !granted(requested, &path, right, &kinds)?
            {
                return Err(PepError::Authorization(format!(
                    "effective permissions widen request at {path}"
                )));
            }
        }
    }
    Ok(())
}

fn granted(
    permissions: &OsPermissionSet,
    path: &str,
    right: FileRight,
    kinds: &BTreeMap<&str, bool>,
) -> Result<bool, PepError> {
    let mut allowed = false;
    for rule in &permissions.filesystem.rules {
        let canonical = std::fs::canonicalize(&rule.path)?;
        let base = canonical
            .to_str()
            .ok_or_else(|| PepError::Unsupported("non-UTF-8 permission path".into()))?;
        let within = path == base
            || (kinds.get(base) == Some(&true)
                && path.starts_with(&format!("{}/", base.trim_end_matches('/'))));
        if !within {
            continue;
        }
        let applies = match rule.effect {
            RuleEffect::Allow => {
                rule.rights.contains(&right)
                    || (right == FileRight::Read && rule.rights.contains(&FileRight::Write))
            }
            RuleEffect::Deny => {
                rule.rights.contains(&right)
                    || (right == FileRight::Write && rule.rights.contains(&FileRight::Read))
            }
        };
        if applies {
            if rule.effect == RuleEffect::Deny {
                return Ok(false);
            }
            allowed = true;
        }
    }
    Ok(allowed)
}
