//! Store a planning pass once; expand shared statements only for the requested policy.
use std::collections::{BTreeMap, BTreeSet};

use asc_foundation_types::{ResourceId, Revision};

use super::{Derived, Group, Include, Members};
use crate::AutomaticDiagnostic;

pub(crate) enum ScopeOutcome {
    Assigned,
    WaitingForProcess,
    Failed(String),
}

struct ScopeReport {
    id: ResourceId,
    group: Group,
    bound: Members,
    outcome: ScopeOutcome,
}

#[derive(Default)]
pub(crate) struct PlanningDiagnostics {
    policy_indices: BTreeMap<(ResourceId, Revision), usize>,
    scopes: Vec<ScopeReport>,
    includes: Vec<Include>,
    unassigned: Vec<AutomaticDiagnostic>,
}

impl Derived {
    /// Move the same membership data used for deployment into the completed pass.
    pub(crate) fn into_diagnostics(
        mut self,
        outcomes: Vec<(ResourceId, ScopeOutcome)>,
    ) -> PlanningDiagnostics {
        let policy_indices = self
            .policies
            .iter()
            .enumerate()
            .map(|(index, p)| ((p.policy_id.clone(), p.revision), index))
            .collect();
        let scopes = outcomes
            .into_iter()
            .filter_map(|(id, outcome)| {
                // A transfer may also contain a retired Scope without a desired assignment.
                let scope = self.scopes.remove(&id)?;
                Some(ScopeReport {
                    id,
                    group: scope.group,
                    bound: scope.bound,
                    outcome,
                })
            })
            .collect();
        PlanningDiagnostics {
            policy_indices,
            scopes,
            includes: self.includes,
            unassigned: self.diagnostics,
        }
    }
}

impl PlanningDiagnostics {
    pub(crate) fn for_policy(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Vec<AutomaticDiagnostic> {
        let Some(&index) = self.policy_indices.get(&(id.clone(), revision)) else {
            return Vec::new();
        };
        let mut rows: Vec<_> = self
            .unassigned
            .iter()
            .filter(|row| row.policy_id == *id && row.revision == revision)
            .cloned()
            .collect();
        let includes: Vec<_> = self
            .includes
            .iter()
            .filter(|rule| rule.policy == index)
            .collect();
        for scope in &self.scopes {
            let mut statement_ids: BTreeSet<_> =
                scope.bound.get(&index).cloned().unwrap_or_default();
            statement_ids.extend(
                includes
                    .iter()
                    .filter(|rule| rule.applies_to(&scope.group))
                    .map(|rule| rule.statement.clone()),
            );
            if statement_ids.is_empty() {
                continue;
            }
            let (state, reason) = match &scope.outcome {
                ScopeOutcome::Assigned => ("assigned", None),
                ScopeOutcome::WaitingForProcess => ("waitingForProcess", None),
                ScopeOutcome::Failed(reason) => ("failed", Some(reason.clone())),
            };
            rows.push(AutomaticDiagnostic {
                policy_id: id.clone(),
                revision,
                statement_ids,
                backend: scope.group.backend,
                scope_id: Some(scope.id.clone()),
                state: state.into(),
                reason,
            });
        }
        rows
    }
}

#[cfg(test)]
mod tests;
