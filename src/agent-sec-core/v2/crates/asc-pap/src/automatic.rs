//! Derive Scope membership from current PAP sources and a fixed integration configuration.
use std::collections::{BTreeMap, BTreeSet};

use asc_foundation_types::{ResourceId, Revision};
use asc_policy_cedar::deployment::{self, Placement, StatementTarget};
use asc_policy_cedar::selection::SelectedStatement;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::policy::{DeploymentBackend, DeploymentSelection, PreparedPolicy};
use asc_policy_types::scope::{PreparedScope, ProcessMatcher, ScopeSelector, ScopeStatus};
use sha2::{Digest, Sha224};
use std::sync::Arc;

use crate::{AutomaticDiagnostic, PapError};

mod diagnostics;
pub(super) use diagnostics::{PlanningDiagnostics, ScopeOutcome};

/// Every Cedar source can contribute a process target or a restriction on another source.
pub fn is_automatic_source(policy: &PreparedPolicy) -> bool {
    matches!(&policy.template, PolicySource::Cedar(_))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Group {
    backend: DeploymentBackend,
    matcher: ProcessMatcher,
    uid: Option<u32>,
}

impl Group {
    fn id(&self) -> Result<ResourceId, PapError> {
        // Include all selection criteria, allowing retries to find the same candidate
        // even if the initial admission failed before any Scope was persisted.
        let key = serde_json::to_vec(&(self.backend, &self.matcher, self.uid))
            .map_err(|_| PapError::Persistence)?;
        let digest = Sha224::digest(key);
        // Credential Scope IDs are DNS labels and must fit within 63 bytes.
        ResourceId::new(format!("auto-{digest:x}"))
            .map_err(|e| PapError::InvalidIdentifier(e.to_string()))
    }
}

type Members = BTreeMap<usize, BTreeSet<String>>;
type Groups = BTreeMap<Group, (Members, BTreeSet<Grant>)>;

pub(super) struct Candidate {
    group: Group,
    bound: Members,
    grants: BTreeSet<Grant>,
}

struct Include {
    backend: DeploymentBackend,
    target: Option<(ProcessMatcher, Option<u32>)>,
    policy: usize,
    statement: String,
}

impl Include {
    fn applies_to(&self, group: &Group) -> bool {
        self.backend == group.backend
            && self.target.as_ref().is_none_or(|(matcher, uid)| {
                matcher == &group.matcher && (uid.is_none() || *uid == group.uid)
            })
    }
}

pub(super) struct Derived {
    policies: Arc<Vec<PreparedPolicy>>,
    views: crate::selection::DeploymentViews,
    source_bytes: Vec<usize>,
    includes: Vec<Include>,
    pub sources: Vec<(ResourceId, Revision)>,
    pub scopes: BTreeMap<ResourceId, Candidate>,
    pub diagnostics: Vec<AutomaticDiagnostic>,
}

impl Derived {
    fn members(&self, scope: &Candidate) -> Members {
        let mut members = scope.bound.clone();
        for rule in &self.includes {
            if rule.applies_to(&scope.group) {
                members
                    .entry(rule.policy)
                    .or_default()
                    .insert(rule.statement.clone());
            }
        }
        members
    }

    fn fragments(
        &self,
        backend: DeploymentBackend,
        index: usize,
        ids: &BTreeSet<String>,
    ) -> BTreeMap<String, String> {
        let Some((_, view)) = self
            .views
            .iter()
            .find(|(selector, _)| selector.backend == backend)
        else {
            return BTreeMap::new();
        };
        let Some((_, rows)) = view.policy(index) else {
            return BTreeMap::new();
        };
        rows.iter()
            .filter_map(|row| {
                let id = &row.source.statement_id;
                match &row.selected {
                    SelectedStatement::Fragment { cedar, .. } if ids.contains(id) => {
                        Some((id.clone(), cedar.clone()))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    fn unchanged(&self, old: &PreparedScope, planned: &Candidate) -> bool {
        let members = self.members(planned);
        members.len() == old.policy_snapshots.len()
            && members
                .iter()
                .zip(&old.policy_snapshots)
                .all(|((index, ids), saved)| {
                    let source = &self.policies[*index];
                    saved.policy_id == source.policy_id
                        && saved.revision == source.revision
                        && saved.deployment.as_ref().is_some_and(|selection| {
                            selection.statement_ids == *ids
                                && selection.fragments
                                    == self.fragments(planned.group.backend, *index, ids)
                        })
                })
    }

    pub(super) fn prepare(&self, id: &ResourceId) -> Result<PreparedScope, PapError> {
        let scope = self.scopes.get(id).ok_or(PapError::NotFound)?;
        let mut snapshots = Vec::new();
        let mut bytes = 0;
        for (index, statement_ids) in self.members(scope) {
            // The projections add fields to the source. Reject once the sources alone
            // exceed the existing limit; storage verifies the final encoded assignment.
            bytes += self.source_bytes[index];
            if bytes > crate::MAX_SCOPE_BYTES {
                return Err(PapError::InvalidScope(
                    asc_policy_types::ValidationError::new(
                        "policyTemplates",
                        "serialized assignment exceeds 1 MiB",
                    ),
                ));
            }
            let mut policy = self.policies[index].clone();
            let selection = DeploymentSelection {
                backend: scope.group.backend,
                fragments: self.fragments(scope.group.backend, index, &statement_ids),
                statement_ids,
            };
            if let Some(validate) = self
                .views
                .iter()
                .find(|(selector, _)| selector.backend == scope.group.backend)
                .and_then(|(selector, _)| selector.validate)
                && let PolicySource::Cedar(source) = &policy.template
            {
                let view = deployment::selected_pst(source, &selection)?;
                validate(&view).map_err(|error| {
                    PapError::InvalidCedar(format!(
                        "{} revision {}: {}",
                        policy.policy_id,
                        policy.revision.get(),
                        error.code
                    ))
                })?;
            }
            policy.deployment = Some(selection);
            snapshots.push(policy);
        }
        Ok(PreparedScope {
            scope_id: id.clone(),
            selector: ScopeSelector::Process {
                matcher: scope.group.matcher.clone(),
            },
            policy_snapshots: snapshots,
            owner_uid: scope.group.uid,
            status: ScopeStatus::Active,
        })
    }

    pub(super) fn prepare_publication(
        &self,
        updates: &mut ScopePublication,
    ) -> Result<(), PapError> {
        // A transfer must admit all destinations together. Keep this failure local to its group.
        if updates
            .keys()
            .filter(|id| self.scopes.contains_key(*id))
            .count()
            > crate::MAX_ACTIVE_SCOPES
        {
            return Err(PapError::Unavailable);
        }
        for (id, update) in updates {
            if let Some(planned) = self.scopes.get(id) {
                update.candidate = Some(match &update.expected {
                    Some(saved)
                        if saved.status == ScopeStatus::Active
                            && self.unchanged(saved, planned) =>
                    {
                        saved.clone()
                    }
                    _ => self.prepare(id)?,
                });
            }
        }
        Ok(())
    }
}

pub(super) fn derive(
    policies: Arc<Vec<PreparedPolicy>>,
    matchers: Result<&BTreeMap<String, ProcessMatcher>, &String>,
    views: crate::selection::DeploymentViews,
) -> Result<Derived, PapError> {
    let mut diagnostics = Vec::new();
    let mut sources = Vec::new();
    let mut source_bytes = vec![0; policies.len()];
    // Each bound statement creates a group. Other statements can only add rules
    // to groups already established by a backend selector.
    let mut groups = Groups::new();
    let mut includes = Vec::new();
    for (index, policy) in policies
        .iter()
        .enumerate()
        .filter(|(_, p)| is_automatic_source(p))
    {
        sources.push((policy.policy_id.clone(), policy.revision));
        source_bytes[index] = serde_json::to_vec(policy)
            .map_err(|_| PapError::Persistence)?
            .len();
        for (selector, view) in &views {
            let Some((parsed, selections)) = view.policy(index) else {
                continue;
            };
            let backend = selector.backend;
            for (rule, row) in parsed.statements.iter().zip(selections) {
                let mut diagnose = |reason: String| {
                    diagnostics.push(AutomaticDiagnostic {
                        policy_id: policy.policy_id.clone(),
                        revision: policy.revision,
                        statement_ids: BTreeSet::from([rule.id.clone()]),
                        backend,
                        scope_id: None,
                        state: "unassigned".into(),
                        reason: Some(reason),
                    });
                };
                if let SelectedStatement::Unsupported(reason) = &row.selected
                    && !matches!((selector.place)(rule), Placement::Ignore)
                {
                    diagnose(reason.clone());
                }
                if !row.selected.is_selected() {
                    continue;
                }
                let placement = (selector.place)(rule);
                let matchers = match matchers {
                    Ok(matchers) => matchers,
                    Err(reason) => {
                        if !matches!(placement, Placement::Ignore) {
                            diagnose(reason.clone());
                        }
                        continue;
                    }
                };
                let subject = match placement {
                    Placement::Ignore => continue,
                    Placement::Unassigned(reason) => {
                        diagnose(reason);
                        continue;
                    }
                    Placement::Bind(subject) | Placement::Include(Some(subject)) => subject,
                    Placement::Include(None) => {
                        includes.push(Include {
                            backend,
                            target: None,
                            policy: index,
                            statement: rule.id.clone(),
                        });
                        continue;
                    }
                };
                let Some(matcher) = matchers.get(&subject.agent_type) else {
                    diagnose(format!(
                        "no process matcher configured for agentType {}",
                        subject.agent_type
                    ));
                    continue;
                };
                if matches!(placement, Placement::Bind(_)) {
                    let group = Group {
                        backend,
                        matcher: matcher.clone(),
                        uid: subject.owner_uid,
                    };
                    bind_rule(&mut groups, group, index, policy, rule);
                } else {
                    includes.push(Include {
                        backend,
                        target: Some((matcher.clone(), subject.owner_uid)),
                        policy: index,
                        statement: rule.id.clone(),
                    });
                }
            }
        }
    }
    Ok(Derived {
        policies,
        views,
        source_bytes,
        includes,
        sources,
        scopes: index_candidates(groups)?,
        diagnostics,
    })
}

fn bind_rule(
    groups: &mut Groups,
    group: Group,
    index: usize,
    policy: &PreparedPolicy,
    rule: &StatementTarget,
) {
    let credential = group.backend == DeploymentBackend::Gateway;
    let (members, grants) = groups.entry(group).or_default();
    members.entry(index).or_default().insert(rule.id.clone());
    if credential && let Some(grant) = &rule.credential_rule {
        add_grant(grants, policy, grant.clone());
    }
}

fn index_candidates(groups: Groups) -> Result<BTreeMap<ResourceId, Candidate>, PapError> {
    let mut scopes = BTreeMap::new();
    for (group, (bound, grants)) in groups {
        scopes.insert(
            group.id()?,
            Candidate {
                group,
                bound,
                grants,
            },
        );
    }
    Ok(scopes)
}

/// An exact serving assignment and the desired assignment that replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeUpdate {
    /// Currently serving assignment, absent on first publication.
    pub expected: Option<PreparedScope>,
    /// Next assignment, absent when the source transfers its final grant.
    pub candidate: Option<PreparedScope>,
}

/// Scopes participating in one transfer must change membership together.
pub type ScopePublication = BTreeMap<ResourceId, ScopeUpdate>;

/// Moves each participating serving snapshot into its publication's expected-state check.
pub(super) fn take_publication(
    ids: Vec<ResourceId>,
    current: &mut BTreeMap<ResourceId, PreparedScope>,
) -> ScopePublication {
    ids.into_iter()
        .map(|id| {
            let expected = current.remove(&id);
            (
                id,
                ScopeUpdate {
                    expected,
                    candidate: None,
                },
            )
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Grant {
    Rule(ResourceId, String),
    Pair(String),
}

fn add_grant(grants: &mut BTreeSet<Grant>, policy: &PreparedPolicy, rule: String) {
    if let Some(pair) = policy
        .credential_bindings
        .iter()
        .find(|entry| entry.rule_id == rule)
    {
        // Preserve pair membership even when a move renames its source rule.
        grants.insert(Grant::Pair(pair.pair_id.clone()));
    }
    grants.insert(Grant::Rule(policy.policy_id.clone(), rule));
}

fn grants(scope: &PreparedScope) -> Result<BTreeSet<Grant>, PapError> {
    let mut grants = BTreeSet::new();
    for policy in &scope.policy_snapshots {
        let (Some(selected), PolicySource::Cedar(source)) = (&policy.deployment, &policy.template)
        else {
            continue;
        };
        for stmt in
            deployment::statements(source).map_err(|e| PapError::InvalidCedar(e.diagnostic()))?
        {
            if selected.statement_ids.contains(&stmt.id)
                && let Some(rule) = stmt.credential_rule
            {
                add_grant(&mut grants, policy, rule);
            }
        }
    }
    Ok(grants)
}

/// Pair a newly acquired grant with the Scope that lost it. An ordinary unannotated
/// restriction can occur in several Scopes but cannot cause a credential transfer.
pub(super) fn publications(
    current: &BTreeMap<ResourceId, PreparedScope>,
    desired: &Derived,
) -> Result<Vec<Vec<ResourceId>>, PapError> {
    // Ordinary additions and unchanged Scopes are independent singleton publications.
    let mut neighbors: BTreeMap<_, BTreeSet<ResourceId>> = desired
        .scopes
        .keys()
        .cloned()
        .map(|id| (id, BTreeSet::new()))
        .collect();
    let mut gains = BTreeMap::<Grant, Vec<ResourceId>>::new();
    let mut losses = BTreeMap::<Grant, Vec<ResourceId>>::new();
    for id in current
        .keys()
        .chain(desired.scopes.keys())
        .collect::<BTreeSet<_>>()
    {
        let old = current.get(id).filter(|s| {
            s.automatic_backend() == Some(DeploymentBackend::Gateway)
                && s.status == ScopeStatus::Active
        });
        let new = desired.scopes.get(id);
        if old
            .zip(new)
            .is_some_and(|(scope, plan)| desired.unchanged(scope, plan))
        {
            continue;
        }
        let before = old.map(grants).transpose()?.unwrap_or_default();
        let empty = BTreeSet::new();
        let after = new.map_or(&empty, |scope| &scope.grants);
        for grant in after.difference(&before) {
            gains.entry(grant.clone()).or_default().push(id.clone());
        }
        for grant in before.difference(after) {
            losses.entry(grant.clone()).or_default().push(id.clone());
        }
    }
    for (grant, destinations) in gains {
        if let Some(origins) = losses.get(&grant)
            && let Some((first, rest)) = origins.split_first()
        {
            // A star connects the same transfer without allocating an edge for every pair.
            for peer in rest.iter().chain(&destinations) {
                neighbors
                    .entry(first.clone())
                    .or_default()
                    .insert(peer.clone());
                neighbors
                    .entry(peer.clone())
                    .or_default()
                    .insert(first.clone());
            }
        }
    }

    let mut groups = Vec::new();
    while let Some(first) = neighbors.keys().next().cloned() {
        let mut group = Vec::new();
        let mut pending = vec![first];
        while let Some(id) = pending.pop() {
            if let Some(related) = neighbors.remove(&id) {
                pending.extend(related);
                group.push(id);
            }
        }
        groups.push(group);
    }
    Ok(groups)
}

#[cfg(test)]
mod tests;
