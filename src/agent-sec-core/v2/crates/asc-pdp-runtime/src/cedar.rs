//! Uses the shared frontend and Cedar evaluator without replacing their semantics.

use std::collections::BTreeSet;

use asc_policy_cedar::advice::Advice;
use asc_policy_types::cedar::AuthorizationPolicy;
use cedar_policy::{Authorizer, Effect, PolicySet};

use crate::{PdpError, path_conditions, request::Query};

pub(crate) struct CompiledPolicySet {
    pub policies: PolicySet,
    pub boundaries: BTreeSet<String>,
}

pub(crate) struct Outcome {
    pub allowed: bool,
    pub forbidden: bool,
    pub advice: Vec<Advice>,
}

impl CompiledPolicySet {
    pub fn from_members(members: &[AuthorizationPolicy]) -> Result<Self, PdpError> {
        let mut policies = PolicySet::new();
        for member in members {
            if !asc_policy_cedar::request_authorization_eligible(&member.source)? {
                return Err(PdpError::Configuration(format!(
                    "{} has bare principal headers",
                    member.policy_id
                )));
            }
            let source = PolicySet::from_pst(asc_policy_cedar::pst(&member.source)?)
                .map_err(|error| PdpError::Evaluation(error.to_string()))?;
            for policy in source.policies() {
                // Length-prefix the repository ID so even IDs containing separators cannot collide.
                let id = format!(
                    "{}:{}:{}:{}",
                    member.policy_id.as_str().len(),
                    member.policy_id,
                    member.revision.get(),
                    policy.id()
                );
                policies
                    .add(policy.new_id(cedar_policy::PolicyId::new(id)))
                    .map_err(|error| PdpError::Evaluation(error.to_string()))?;
            }
        }
        let boundaries = path_conditions::boundaries(&policies)?;
        Ok(Self {
            policies,
            boundaries,
        })
    }

    pub fn evaluate(&self, query: &Query) -> Result<Outcome, PdpError> {
        let response =
            Authorizer::new().is_authorized(&query.request, &self.policies, &query.entities);
        let errors: Vec<_> = response
            .diagnostics()
            .errors()
            .map(ToString::to_string)
            .collect();
        if !errors.is_empty() {
            return Err(PdpError::Evaluation(errors.join("; ")));
        }
        let mut outcome = Outcome {
            allowed: response.decision() == cedar_policy::Decision::Allow,
            forbidden: false,
            advice: Vec::new(),
        };
        for id in response.diagnostics().reason() {
            let policy = self
                .policies
                .policy(id)
                .ok_or_else(|| PdpError::Evaluation(format!("missing diagnostic policy {id}")))?;
            if policy.effect() == Effect::Forbid {
                outcome.forbidden = true;
            }
            if let Some(value) = policy.annotation("advice") {
                outcome.advice.push(
                    Advice::parse(value)
                        .map_err(|error| PdpError::Evaluation(error.to_string()))?,
                );
            }
        }
        Ok(outcome)
    }
}
