//! Uses the shared frontend and Cedar evaluator without replacing their semantics.

use std::collections::BTreeSet;

use asc_policy_cedar::advice::Advice;
use asc_policy_cedar::selection::{SelectedStatement, Selection, ValidatedStatement};
use asc_policy_types::cedar::{AuthorizationPolicy, CedarSchemaReference};
use asc_policy_types::identifiers::{ResourceId, Revision};
use cedar_policy::{Authorizer, Effect, Policy, PolicyId, PolicySet};

use crate::{PdpError, path_conditions, request::Query};

#[derive(Default)]
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
        let mut compiled = Self::default();
        for member in members {
            let source = PolicySet::from_pst(asc_policy_cedar::pst(&member.source)?)
                .map_err(|error| PdpError::Evaluation(error.to_string()))?;
            if !asc_policy_cedar::request_policy_headers_for_schema(&source, &member.source.schema)?
            {
                return Err(PdpError::Configuration(format!(
                    "{} has bare principal headers",
                    member.policy_id
                )));
            }
            for policy in source.policies() {
                compiled.add(
                    &member.policy_id,
                    member.revision,
                    &member.source.schema,
                    policy,
                )?;
            }
        }
        Ok(compiled)
    }

    pub fn from_selected<'a>(
        statements: impl IntoIterator<Item = (ValidatedStatement<'a>, &'a Selection)>,
    ) -> Result<Self, PdpError> {
        let mut compiled = Self::default();
        for (statement, choice) in statements {
            if &choice.source.policy_id != statement.policy_id
                || choice.source.revision != statement.revision
                || choice.source.statement_id != statement.rule.id
            {
                return Err(PdpError::Configuration(format!(
                    "selection does not belong to {} revision {} statement {}",
                    statement.policy_id,
                    statement.revision.get(),
                    statement.rule.id
                )));
            }
            let fragment;
            let policy = match &choice.selected {
                SelectedStatement::Whole | SelectedStatement::Undetermined => {
                    statement.rule.policy()
                }
                SelectedStatement::Fragment { cedar, .. } => {
                    fragment = Policy::parse(Some(PolicyId::new(&statement.rule.id)), cedar)
                        .map_err(|error| PdpError::Evaluation(error.to_string()))?;
                    &fragment
                }
                SelectedStatement::Unrelated | SelectedStatement::Unsupported(_) => continue,
            };
            compiled.add(
                statement.policy_id,
                statement.revision,
                statement.schema,
                policy,
            )?;
        }
        Ok(compiled)
    }

    fn add(
        &mut self,
        policy_id: &ResourceId,
        revision: Revision,
        schema: &CedarSchemaReference,
        policy: &Policy,
    ) -> Result<(), PdpError> {
        // Preserve the original statement ID even when sibling branches or statements were omitted.
        let id = format!(
            "{}:{}:{}:{}",
            policy_id.as_str().len(),
            policy_id,
            revision.get(),
            policy.id()
        );
        let policy = policy.new_id(PolicyId::new(id));
        path_conditions::collect_boundaries(&policy, schema, &mut self.boundaries)?;
        self.policies
            .add(policy)
            .map_err(|error| PdpError::Evaluation(error.to_string()))
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
