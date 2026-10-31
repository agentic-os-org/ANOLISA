//! Coordinates five-state outcomes while preserving every matching requirement.

use asc_policy_cedar::advice::{Advice, AdviceVerdict, Transform};
use asc_policy_types::{
    AgentPrincipal, AuthorizationRequest, Decision, DecisionVerdict, LaunchSpec, OsPermissionSet,
    Validate,
};

use crate::{PdpError, PdpRuntime, request};

const MAX_CONTINUATIONS: usize = 1024;

#[derive(Clone)]
pub(crate) struct Continuation {
    principal: AgentPrincipal,
    execution_id: String,
    candidate: LaunchSpec,
    transform: Transform,
    requested_permissions: OsPermissionSet,
    approval: bool,
    approval_reasons: Vec<String>,
    warnings: Vec<String>,
    expires_at: u64,
}

#[derive(Default)]
struct Requirements {
    warnings: Vec<String>,
    approval: bool,
    approval_reasons: Vec<String>,
    transform: Option<Transform>,
}

impl PdpRuntime {
    pub(crate) fn evaluate_request(
        &self,
        input: &AuthorizationRequest,
        now: u64,
    ) -> Result<Decision, PdpError> {
        let mut decision = self.base_decision(input, now)?;
        if let Err(error) = self.evaluate_final(input, now, &mut decision) {
            decision.verdict = DecisionVerdict::Deny;
            decision.effective_permissions = None;
            decision.reason = error.to_string();
        }
        Ok(decision)
    }

    fn evaluate_final(
        &self,
        input: &AuthorizationRequest,
        now: u64,
        decision: &mut Decision,
    ) -> Result<(), PdpError> {
        let mut current = input.clone();
        let mut prior = self.continuation(input, now)?;
        loop {
            let (permissions, advice) = match self.authorize(&current) {
                Ok(result) => result,
                Err(PdpError::MissingFacts(paths)) => {
                    return self.defer(decision, prior, paths, now);
                }
                Err(error) => return Err(error),
            };
            let mut requirements = Requirements::collect(advice)?;
            if let Some(prior) = &prior {
                requirements.merge_prior(prior)?;
            }
            requirements.warnings.sort();
            requirements.warnings.dedup();
            decision.warnings.clone_from(&requirements.warnings);
            if requirements.approval && !input.capabilities.ask {
                return Err(PdpError::Denied(
                    "approval is required but this PEP has no confirmation channel".into(),
                ));
            }
            if let Some(transform) = requirements.transform.take() {
                let mut candidate = transform_candidate(&current.launch, &transform)?;
                let prepared = prepare_candidate(&current, &mut candidate)?;
                prior = Some(Continuation {
                    principal: input.principal.clone(),
                    execution_id: input.execution_id.clone(),
                    candidate: candidate.clone(),
                    transform,
                    requested_permissions: input.requested_permissions.clone(),
                    approval: requirements.approval,
                    approval_reasons: requirements.approval_reasons,
                    warnings: requirements.warnings,
                    expires_at: decision.expires_at,
                });
                if !prepared {
                    return self.defer(decision, prior, Vec::new(), now);
                }
                current.launch = candidate;
                // Authorize the final command under the same policy and permission ceiling.
                continue;
            }
            decision.launch = current.launch;
            decision.effective_permissions = Some(permissions);
            decision.verdict = if requirements.approval {
                DecisionVerdict::StepUp
            } else if prior.is_some() {
                DecisionVerdict::Modify
            } else {
                DecisionVerdict::Allow
            };
            decision.reason = match decision.verdict {
                DecisionVerdict::StepUp => "the final invocation requires explicit confirmation",
                DecisionVerdict::Modify => "the transformed invocation is authorized",
                _ => "the invocation and OS permissions are authorized",
            }
            .into();
            if requirements.approval && !requirements.approval_reasons.is_empty() {
                requirements.approval_reasons.sort();
                requirements.approval_reasons.dedup();
                decision.reason = requirements.approval_reasons.join("; ");
            }
            return Ok(());
        }
    }

    fn defer(
        &self,
        decision: &mut Decision,
        prior: Option<Continuation>,
        paths: Vec<String>,
        now: u64,
    ) -> Result<(), PdpError> {
        if let Some(prior) = prior {
            let mut pending = self.continuations.lock().map_err(|_| PdpError::State)?;
            pending.retain(|_, value| value.expires_at > now);
            if pending.len() >= MAX_CONTINUATIONS {
                return Err(PdpError::Denied("too many pending preparations".into()));
            }
            decision.candidate_launch = Some(prior.candidate.clone());
            decision.warnings.clone_from(&prior.warnings);
            pending.insert(decision.decision_id.clone(), prior);
            decision.continuation = Some(decision.decision_id.clone());
        }
        decision.verdict = DecisionVerdict::Defer;
        decision.reason = "additional execution facts are required".into();
        decision.missing_paths = paths;
        Ok(())
    }

    fn authorize(
        &self,
        input: &AuthorizationRequest,
    ) -> Result<(OsPermissionSet, Vec<Advice>), PdpError> {
        let command = request::invocation(input, &self.schema, true)?;
        let executable = request::invocation(input, &self.schema, false)?;
        let mut advice = Vec::new();
        for layer in &self.layers {
            let command = layer.evaluate(&command)?;
            let executable = layer.evaluate(&executable)?;
            if command.forbidden || executable.forbidden {
                return Err(PdpError::Denied("an invocation view matched forbid".into()));
            }
            if !command.allowed && !executable.allowed {
                return Err(PdpError::Denied(
                    "neither process.spawn nor file.exec is permitted".into(),
                ));
            }
            advice.extend(command.advice);
            advice.extend(executable.advice);
        }
        let permissions = self.permissions(input, &mut advice)?;
        Ok((permissions, advice))
    }

    fn base_decision(&self, input: &AuthorizationRequest, now: u64) -> Result<Decision, PdpError> {
        let expires_at = now
            .checked_add(self.snapshot.ttl_seconds)
            .ok_or_else(|| PdpError::Configuration("decision expiry overflow".into()))?;
        Ok(Decision {
            decision_id: uuid::Uuid::new_v4().to_string(),
            request_id: input.request_id.clone(),
            execution_id: input.execution_id.clone(),
            request_digest: input.digest()?,
            policy_version: self.snapshot.version.clone(),
            expires_at,
            principal: input.principal.clone(),
            launch: input.launch.clone(),
            verdict: DecisionVerdict::Deny,
            effective_permissions: None,
            reason: String::new(),
            warnings: Vec::new(),
            candidate_launch: None,
            missing_paths: Vec::new(),
            continuation: None,
        })
    }

    fn continuation(
        &self,
        input: &AuthorizationRequest,
        now: u64,
    ) -> Result<Option<Continuation>, PdpError> {
        let Some(id) = &input.continuation else {
            return Ok(None);
        };
        let mut pending = self.continuations.lock().map_err(|_| PdpError::State)?;
        pending.retain(|_, value| value.expires_at > now);
        let prior = pending
            .get(id)
            .ok_or_else(|| PdpError::Denied("unknown or expired continuation".into()))?;
        let launch = &input.launch;
        let candidate = &prior.candidate;
        let cwd_matches = launch.cwd == candidate.cwd
            || input.path_facts.iter().any(|fact| {
                fact.path == candidate.cwd && fact.is_directory && fact.canonical_path == launch.cwd
            });
        if prior.principal != input.principal
            || prior.execution_id != input.execution_id
            || launch.program != candidate.program
            || launch.args != candidate.args
            || !cwd_matches
            || launch.env != candidate.env
            || input.requested_permissions != prior.requested_permissions
        {
            return Err(PdpError::Denied(
                "continuation does not match its subject and candidate".into(),
            ));
        }
        let claimed = prior.clone();
        // Claim before evaluation so concurrent replays cannot both issue executable decisions.
        pending.remove(id);
        Ok(Some(claimed))
    }
}

impl Requirements {
    fn merge_prior(&mut self, prior: &Continuation) -> Result<(), PdpError> {
        if self
            .transform
            .as_ref()
            .is_some_and(|transform| transform != &prior.transform)
        {
            return Err(PdpError::Denied(
                "a second distinct transform is unsupported".into(),
            ));
        }
        self.transform = None;
        self.approval |= prior.approval;
        self.approval_reasons
            .extend(prior.approval_reasons.iter().cloned());
        self.warnings.extend(prior.warnings.iter().cloned());
        Ok(())
    }

    fn collect(advice: Vec<Advice>) -> Result<Self, PdpError> {
        let mut requirements = Self::default();
        for advice in advice {
            match advice.verdict {
                AdviceVerdict::Warn => requirements.warnings.push(
                    advice
                        .message
                        .or(advice.reason)
                        .unwrap_or_else(|| "policy warning".into()),
                ),
                AdviceVerdict::Escalate => {
                    requirements.approval = true;
                    if let Some(reason) = advice.message.or(advice.reason) {
                        requirements.approval_reasons.push(reason);
                    }
                }
                AdviceVerdict::Transform => {
                    let transform = advice.transform.ok_or_else(|| {
                        PdpError::Evaluation("transform advice has no replacement".into())
                    })?;
                    if requirements
                        .transform
                        .as_ref()
                        .is_some_and(|other| other != &transform)
                    {
                        return Err(PdpError::Denied(
                            "conflicting transform requirements".into(),
                        ));
                    }
                    requirements.transform = Some(transform);
                }
            }
        }
        Ok(requirements)
    }
}

fn transform_candidate(launch: &LaunchSpec, transform: &Transform) -> Result<LaunchSpec, PdpError> {
    let mut candidate = launch.clone();
    match transform.path.as_str() {
        "$target.program" => {
            candidate.program = serde_json::from_value(transform.value.clone())
                .map_err(|error| PdpError::Unsupported(error.to_string()))?;
        }
        "$target.args" => {
            candidate.args = serde_json::from_value(transform.value.clone())
                .map_err(|error| PdpError::Unsupported(error.to_string()))?;
        }
        "$target.cwd" => {
            candidate.cwd = serde_json::from_value(transform.value.clone())
                .map_err(|error| PdpError::Unsupported(error.to_string()))?;
        }
        _ => {
            return Err(PdpError::Unsupported(
                "transform may replace only program, args, or cwd".into(),
            ));
        }
    }
    if candidate.program.is_empty()
        || candidate.program.contains('\0')
        || candidate.args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(PdpError::Unsupported("invalid transformed command".into()));
    }
    crate::path_conditions::check_path(&candidate.cwd)?;
    Ok(candidate)
}

// Resolve only from execution facts supplied by the PEP, never the daemon's own PATH.
fn prepare_candidate(
    input: &AuthorizationRequest,
    candidate: &mut LaunchSpec,
) -> Result<bool, PdpError> {
    let cwd = input
        .path_facts
        .iter()
        .find(|fact| fact.path == candidate.cwd);
    let Some(cwd) = cwd else {
        candidate.resolved_program.clear();
        return Ok(false);
    };
    if !cwd.is_directory {
        return Err(PdpError::Denied(
            "transformed cwd is not a directory".into(),
        ));
    }
    let program = if candidate.program == input.launch.program && candidate.cwd == input.launch.cwd
    {
        input.launch.resolved_program.clone()
    } else if candidate.program.starts_with('/') {
        candidate.program.clone()
    } else {
        candidate.resolved_program.clear();
        return Ok(false);
    };
    let Some(program) = input.path_facts.iter().find(|fact| fact.path == program) else {
        candidate.resolved_program.clear();
        return Ok(false);
    };
    if program.is_directory {
        return Err(PdpError::Denied(
            "transformed program is a directory".into(),
        ));
    }
    // Canonical identities are also checked locally immediately before execution.
    candidate.cwd.clone_from(&cwd.canonical_path);
    candidate
        .resolved_program
        .clone_from(&program.canonical_path);
    candidate.validate()?;
    Ok(true)
}
