//! Coordinates five-state outcomes while preserving every matching requirement.

use asc_daemon_protocol::authorization::{
    ApprovalRequirement, ApprovalTarget, AuthorizationRequest, CommandRequest, Decision,
    DecisionVerdict, Validate,
};
use asc_policy_cedar::advice::{Advice, AdviceVerdict, Transform};

use crate::{PdpError, PdpRuntime, os_permissions::Projection, request};

pub(crate) type ScopedAdvice = (ApprovalTarget, Advice);

#[derive(Default)]
struct Requirements {
    warnings: Vec<String>,
    approval: bool,
    approval_reasons: Vec<String>,
    approval_items: Vec<ApprovalRequirement>,
    transform: Option<Transform>,
}

impl PdpRuntime {
    pub(crate) fn evaluate_request(
        &self,
        input: &AuthorizationRequest,
        now: u64,
    ) -> Result<Decision, PdpError> {
        let mut decision = self.base_decision(input, now)?;
        if let Err(error) = self.evaluate_final(input, &mut decision) {
            decision.verdict = DecisionVerdict::Deny;
            decision.effective_permissions = None;
            decision.reason = error.to_string();
        }
        Ok(decision)
    }

    fn evaluate_final(
        &self,
        input: &AuthorizationRequest,
        decision: &mut Decision,
    ) -> Result<(), PdpError> {
        let mut current = input.clone();
        let (mut projection, advice) = self.authorize(&current)?;
        let mut requirements = Requirements::collect(advice)?;
        if let Some(transform) = requirements.transform.take() {
            let mut candidate = transform_candidate(&current.launch, &transform)?;
            let program_changed = candidate.program != current.launch.program;
            if (program_changed || candidate.cwd != current.launch.cwd)
                && !candidate.program.starts_with('/')
            {
                decision.verdict = DecisionVerdict::Defer;
                decision.warnings = requirements.warnings;
                decision.reason = "changed command requires local program resolution".into();
                return Ok(());
            }
            if program_changed {
                candidate.resolved_program.clone_from(&candidate.program);
            }
            candidate.validate()?;
            if candidate != current.launch {
                current.launch = candidate;
                let (final_projection, final_advice) = self.authorize(&current)?;
                projection = final_projection;
                let final_requirements = Requirements::collect(final_advice)?;
                if final_requirements
                    .transform
                    .as_ref()
                    .is_some_and(|next| next != &transform)
                {
                    return Err(PdpError::Denied(
                        "a second distinct transform is unsupported".into(),
                    ));
                }
                requirements.approval |= final_requirements.approval;
                // An original requirement still gates execution, but is not a final resource grant.
                for item in &mut requirements.approval_items {
                    item.target = ApprovalTarget::Execution {
                        original_request: true,
                    };
                }
                requirements
                    .approval_items
                    .extend(final_requirements.approval_items);
                requirements.warnings.extend(final_requirements.warnings);
                requirements
                    .approval_reasons
                    .extend(final_requirements.approval_reasons);
            }
        }
        requirements.warnings.sort();
        requirements.warnings.dedup();
        decision.warnings = requirements.warnings;
        decision.verdict = if requirements.approval {
            DecisionVerdict::StepUp
        } else if current.launch != input.launch || projection.narrowed {
            DecisionVerdict::Modify
        } else {
            DecisionVerdict::Allow
        };
        decision.launch = current.launch;
        decision.require_files = projection.require_files;
        decision.effective_permissions = Some(projection.permissions);
        requirements.approval_items.sort();
        requirements.approval_items.dedup();
        decision.approval_requirements = requirements.approval_items;
        decision.reason = match decision.verdict {
            DecisionVerdict::StepUp => "the final invocation requires explicit confirmation",
            DecisionVerdict::Modify => "the final command or permissions differ from the request",
            _ => "the invocation and OS permissions are authorized",
        }
        .into();
        if requirements.approval && !requirements.approval_reasons.is_empty() {
            requirements.approval_reasons.sort();
            requirements.approval_reasons.dedup();
            decision.reason = requirements.approval_reasons.join("; ");
        }
        Ok(())
    }

    fn authorize(
        &self,
        input: &AuthorizationRequest,
    ) -> Result<(Projection, Vec<ScopedAdvice>), PdpError> {
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
            advice.extend(
                command
                    .advice
                    .into_iter()
                    .chain(executable.advice)
                    .map(|value| {
                        (
                            ApprovalTarget::Execution {
                                original_request: false,
                            },
                            value,
                        )
                    }),
            );
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
            approval_requirements: Vec::new(),
            warnings: Vec::new(),
            require_files: Vec::new(),
        })
    }
}

impl Requirements {
    fn collect(advice: Vec<ScopedAdvice>) -> Result<Self, PdpError> {
        let mut requirements = Self::default();
        for (target, advice) in advice {
            match advice.verdict {
                AdviceVerdict::Warn => requirements.warnings.push(
                    advice
                        .message
                        .or(advice.reason)
                        .unwrap_or_else(|| "policy warning".into()),
                ),
                AdviceVerdict::Escalate => {
                    requirements.approval = true;
                    requirements.approval_items.push(ApprovalRequirement {
                        target,
                        message: advice
                            .message
                            .clone()
                            .filter(|value| !value.trim().is_empty()),
                    });
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

fn transform_candidate(
    launch: &CommandRequest,
    transform: &Transform,
) -> Result<CommandRequest, PdpError> {
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
