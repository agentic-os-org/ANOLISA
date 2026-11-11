//! Optional upper PEP flow: prepare, authorize, approve, enforce and report.

use crate::{PepError, SandboxBackend, prepare_launch, prepare_path_facts};
use asc_daemon_protocol::authorization::{
    AcceptedAuthorization, AgentPrincipal, AuthorizationRequest, CommandRequest, Decision,
    DecisionVerdict, ExecutionReceipt, ExecutionResult, ExecutionSpec, FileRight, LaunchSpec,
    NetworkMode, OsPermissionSet, PathFact, ReceiptAck, RequestType, RuleEffect, Validate,
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

    /// Binds source constraints before permissions can reach either execution or the PDP.
    ///
    /// # Errors
    /// Rejects invalid permissions, unresolved objects and changed source constraints.
    fn prepare(&self, launch: &LaunchSpec) -> Result<ExecutionSpec, PepError> {
        let permissions = self.permissions(launch)?;
        let path_facts = prepare_path_facts(launch, &permissions, &[])?;
        self.validate_path_facts(launch, &permissions, &path_facts)?;
        Ok(ExecutionSpec {
            version: 1,
            launch: launch.clone(),
            permissions,
            path_facts,
            mount_aliases: Vec::new(),
        })
    }

    /// Checks source-specific constraints against the same object facts retained for execution.
    ///
    /// # Errors
    /// Rejects facts that would widen the prepared permission source.
    fn validate_path_facts(
        &self,
        _launch: &LaunchSpec,
        _permissions: &OsPermissionSet,
        _facts: &[PathFact],
    ) -> Result<(), PepError> {
        Ok(())
    }
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

/// Invocation kept by the executor while the PDP evaluates the public request.
#[derive(Clone)]
pub struct PreparedInvocation {
    /// Data sent to the PDP.
    pub request: AuthorizationRequest,
    /// Private execution state pinned during initial preparation.
    execution: ExecutionSpec,
}

impl PreparedInvocation {
    /// Pins the command and permission objects before contacting the PDP.
    ///
    /// # Errors
    /// Rejects unresolvable commands, paths, or invalid OS permissions.
    pub fn new(
        principal: &AgentPrincipal,
        launch: LaunchSpec,
        provider: &dyn PermissionProvider,
    ) -> Result<Self, PepError> {
        let launch = prepare_launch(
            &launch.program,
            launch.args,
            Path::new(&launch.cwd),
            launch.env,
        )?;
        let prepared = provider.prepare(&launch)?;
        let mut permissions = prepared.permissions;
        let path_facts = prepared.path_facts;
        let mount_aliases = permissions
            .filesystem
            .rules
            .iter()
            .filter(|rule| rule.effect == RuleEffect::Allow)
            .filter(|rule| {
                path_facts
                    .iter()
                    .any(|f| f.path == rule.path && f.path != f.canonical_path)
            })
            .map(|rule| rule.path.clone())
            .chain(prepared.mount_aliases)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for path in permissions
            .filesystem
            .rules
            .iter_mut()
            .map(|r| &mut r.path)
            .chain(&mut permissions.execution.deny_paths)
        {
            let fact = path_facts
                .iter()
                .find(|fact| fact.path == *path)
                .ok_or_else(|| PepError::Protocol("missing requested path identity".into()))?;
            path.clone_from(&fact.canonical_path);
        }
        let request = AuthorizationRequest {
            request_type: RequestType::Os,
            request_id: uuid::Uuid::new_v4().to_string(),
            execution_id: uuid::Uuid::new_v4().to_string(),
            principal: principal.clone(),
            launch: CommandRequest::from(&launch),
            requested_permissions: permissions.clone(),
        };
        request.validate()?;
        Ok(Self {
            request,
            execution: ExecutionSpec {
                version: 1,
                launch,
                permissions,
                path_facts,
                mount_aliases,
            },
        })
    }
}

/// Upper PEP state; retain one instance across invocations to reject decision replay.
pub struct PepRuntime {
    backend: Box<dyn SandboxBackend>,
    consumed: HashMap<String, u64>,
}

impl PepRuntime {
    /// Uses the deployment-selected enforcement backend.
    pub fn new(backend: impl SandboxBackend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
            consumed: HashMap::new(),
        }
    }

    /// Prepares and authorizes a launch, in one evaluation.
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
        approval: Option<&mut dyn ApprovalHandler>,
    ) -> Result<PepOutcome, PepError> {
        let (request, decision) = self.authorize(principal, launch, provider, client)?;
        self.apply(&request, decision, client, approval)
    }

    /// Prepares a final decision for a host that owns approval and later execution.
    /// The host must accept the decision before expiry and preserve it for its executor.
    ///
    /// # Errors
    /// Returns preparation, transport, or binding failures without execution.
    pub fn authorize(
        &mut self,
        principal: &AgentPrincipal,
        launch: LaunchSpec,
        provider: &dyn PermissionProvider,
        client: &dyn AuthorizationClient,
    ) -> Result<(PreparedInvocation, Decision), PepError> {
        let prepared = PreparedInvocation::new(principal, launch, provider)?;
        let decision = client.evaluate(&prepared.request)?;
        verify_binding(&prepared.request, &decision, now()?)?;
        Ok((prepared, decision))
    }

    /// Pins any new policy boundaries before handing the decision to an approval host.
    ///
    /// # Errors
    /// Rejects a mismatched decision, changed objects, widening or unavailable enforcement.
    pub fn prepare_execution(
        &self,
        prepared: &PreparedInvocation,
        decision: &Decision,
    ) -> Result<ExecutionSpec, PepError> {
        verify_binding(&prepared.request, decision, now()?)?;
        let permissions = decision.effective_permissions.as_ref().ok_or_else(|| {
            PepError::Protocol("executable decision lacks effective permissions".into())
        })?;
        let command = &decision.launch;
        let launch = prepare_launch(
            &command.program,
            command.args.clone(),
            Path::new(&command.cwd),
            prepared.execution.launch.env.clone(),
        )?;
        if CommandRequest::from(&launch) != *command {
            return Err(PepError::Authorization(
                "final launch no longer matches its authorized resolution".into(),
            ));
        }
        let extra: Vec<_> = prepared
            .execution
            .path_facts
            .iter()
            .map(|f| f.path.clone())
            .chain(decision.require_files.iter().cloned())
            .collect();
        let facts = prepare_path_facts(&launch, permissions, &extra)?;
        for original in &prepared.execution.path_facts {
            if !facts.contains(original) {
                return Err(PepError::Authorization(format!(
                    "path identity changed: {}",
                    original.path
                )));
            }
        }
        verify_ceiling(permissions, &prepared.request.requested_permissions, &facts)?;
        let caps = self.backend.capabilities()?;
        if !caps.strict_filesystem
            || (!permissions.execution.deny_paths.is_empty() && !caps.noexec)
            || (permissions.network.mode == NetworkMode::Off && !caps.network_off)
        {
            return Err(PepError::Unsupported(
                "backend lacks a required OS restriction".into(),
            ));
        }
        let spec = ExecutionSpec {
            version: 1,
            launch,
            permissions: permissions.clone(),
            path_facts: facts,
            mount_aliases: prepared.execution.mount_aliases.clone(),
        };
        decision.validate_execution(&spec)?;
        self.backend.preflight(&spec)?;
        Ok(spec)
    }

    /// Accepts a pushed or pulled decision once, before its deadline, for this invocation.
    /// Approval and execution may finish after the acceptance deadline.
    ///
    /// # Errors
    /// Rejects mismatched, expired, repeated, or non-executable decisions.
    pub fn apply(
        &mut self,
        prepared: &PreparedInvocation,
        decision: Decision,
        client: &dyn AuthorizationClient,
        approval: Option<&mut dyn ApprovalHandler>,
    ) -> Result<PepOutcome, PepError> {
        let time = now()?;
        let spec = self.prepare_execution(prepared, &decision);
        self.apply_at(&prepared.request, decision, spec, client, approval, time)
    }

    /// Executes an invocation accepted earlier and transferred once by the trusted daemon.
    /// Callers must obtain this value through the owner-bound claim API, not a local JSON file.
    ///
    /// # Errors
    /// Rejects invalid bindings, changed path identities and unavailable execution capabilities.
    pub fn resume(
        &mut self,
        accepted: AcceptedAuthorization,
        client: &dyn AuthorizationClient,
        approval: Option<&mut dyn ApprovalHandler>,
    ) -> Result<PepOutcome, PepError> {
        self.apply_at(
            &accepted.request,
            accepted.decision,
            Ok(accepted.execution),
            client,
            approval,
            accepted.accepted_at,
        )
    }

    /// Records a failed host-side preflight before offering approval.
    ///
    /// # Errors
    /// Reports transport or invalid receipt acknowledgements.
    pub fn report_failure(
        decision: &Decision,
        client: &dyn AuthorizationClient,
        reason: String,
    ) -> Result<(), PepError> {
        let result = ExecutionResult {
            error: Some(reason),
            ..ExecutionResult::default()
        };
        let ack = client.report(&receipt(decision, None, result, now()?))?;
        if !ack.recorded || ack.decision_id != decision.decision_id {
            return Err(PepError::Protocol(
                "invalid failure receipt acknowledgement".into(),
            ));
        }
        Ok(())
    }

    fn apply_at(
        &mut self,
        request: &AuthorizationRequest,
        decision: Decision,
        spec: Result<ExecutionSpec, PepError>,
        client: &dyn AuthorizationClient,
        approval: Option<&mut dyn ApprovalHandler>,
        time: u64,
    ) -> Result<PepOutcome, PepError> {
        if time > now()? {
            return Err(PepError::Authorization(
                "acceptance time is in the future".into(),
            ));
        }
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
            if matches!(
                decision.verdict,
                DecisionVerdict::Defer | DecisionVerdict::Deny
            ) {
                return Err(PepError::Authorization(decision.reason.clone()));
            }
            let spec = spec?;
            decision.validate_execution(&spec)?;
            if decision.verdict == DecisionVerdict::StepUp {
                let channel = approval.ok_or_else(|| {
                    PepError::Authorization("STEP_UP requires an approval channel".into())
                })?;
                let accepted = channel.ask(&decision)?;
                approved = Some(accepted);
                if !accepted {
                    return Err(PepError::Authorization("approval declined".into()));
                }
            }
            let final_launch = prepare_launch(
                &spec.launch.program,
                spec.launch.args.clone(),
                Path::new(&spec.launch.cwd),
                spec.launch.env.clone(),
            )?;
            if spec.launch != final_launch {
                return Err(PepError::Authorization(
                    "final program resolution changed".into(),
                ));
            }
            self.backend.execute_bound(&spec)
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
    for denied in requested
        .filesystem
        .rules
        .iter()
        .filter(|rule| rule.effect == RuleEffect::Deny)
    {
        // Logical default-deny equivalence does not preserve hardlink checks.
        if !denied.rights.iter().all(|right| {
            effective.filesystem.rules.iter().any(|rule| {
                rule.effect == RuleEffect::Deny
                    && rule.path == denied.path
                    && (rule.rights.contains(right) || rule.rights.contains(&FileRight::Read))
            })
        }) {
            return Err(PepError::Authorization(format!(
                "effective permissions removed requested exclusion at {}",
                denied.path
            )));
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
        let canonical = &rule.path;
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
            if granted(effective, &path, right, &kinds) && !granted(requested, &path, right, &kinds)
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
) -> bool {
    let mut allowed = false;
    for rule in &permissions.filesystem.rules {
        let base = rule.path.as_str();
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
                return false;
            }
            allowed = true;
        }
    }
    allowed
}
