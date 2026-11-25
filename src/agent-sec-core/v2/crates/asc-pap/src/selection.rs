//! Share validated source and each consumer's static selections across PAP clients.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use asc_policy_cedar::deployment::DeploymentSelector;
use asc_policy_cedar::selection::{Select, Selection, ValidatedPolicy, ValidatedStatement};
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::AuthorizationPolicy;
use asc_policy_types::policy::{DeploymentBackend, PreparedPolicy};
use sha2::{Digest, Sha256};

use crate::{PapError, PapRepository, PolicySnapshot, PolicySnapshotKind};

/// Statements selected by a single consumer from a consistent, ordered PAP snapshot.
pub struct ConsumerPolicySet {
    /// Repository generation used for invalidation, including changes outside this consumer.
    pub generation: u64,
    sources: Arc<Sources>,
    select: Select,
    selections: Vec<Vec<Selection>>,
}

impl ConsumerPolicySet {
    /// Original statements paired with this consumer's selection, including empty selections.
    pub fn iter(&self) -> impl Iterator<Item = (ValidatedStatement<'_>, &Selection)> {
        self.sources
            .policies
            .iter()
            .zip(&self.sources.parsed)
            .zip(&self.selections)
            .flat_map(|((policy, parsed), selections)| {
                let schema = match &policy.template {
                    PolicySource::Cedar(source) => Some(&source.schema),
                    PolicySource::SecCore(_) => None,
                };
                schema
                    .into_iter()
                    .zip(parsed)
                    .flat_map(move |(schema, parsed)| {
                        parsed
                            .statements
                            .iter()
                            .zip(selections)
                            .map(move |(rule, selection)| {
                                (
                                    ValidatedStatement {
                                        policy_id: &policy.policy_id,
                                        revision: policy.revision,
                                        schema,
                                        rule,
                                    },
                                    selection,
                                )
                            })
                    })
            })
    }

    /// Preserve the receipt digest of the ordered, request-eligible source policies.
    ///
    /// # Errors
    /// Returns a persistence error if the canonical representation cannot be serialized.
    pub fn authorization_version(&self) -> Result<String, PapError> {
        let policies: Vec<_> = self
            .sources
            .policies
            .iter()
            .zip(&self.sources.parsed)
            .filter_map(|(policy, parsed)| {
                let PolicySource::Cedar(source) = &policy.template else {
                    return None;
                };
                (parsed.as_ref()?.request_eligible == Ok(true)).then(|| AuthorizationPolicy {
                    policy_id: policy.policy_id.clone(),
                    revision: policy.revision,
                    source: source.clone(),
                })
            })
            .collect();
        authorization_digest(&policies)
    }

    pub(super) fn policy(&self, index: usize) -> Option<(&ValidatedPolicy, &[Selection])> {
        Some((
            self.sources.parsed[index].as_deref()?,
            &self.selections[index],
        ))
    }

    fn build(sources: Arc<Sources>, select: Select, sandbox: bool) -> Result<Self, PapError> {
        let mut selections = Vec::with_capacity(sources.policies.len());
        for (policy, parsed) in sources.policies.iter().zip(&sources.parsed) {
            let mut rows = Vec::new();
            if let (PolicySource::Cedar(source), Some(parsed)) = (&policy.template, parsed) {
                if sandbox
                    && !*parsed.request_eligible.as_ref().map_err(|reason| {
                        PapError::InvalidCedar(format!(
                            "{} revision {}: {reason}",
                            policy.policy_id,
                            policy.revision.get()
                        ))
                    })?
                {
                    selections.push(rows);
                    continue;
                }
                for rule in &parsed.statements {
                    rows.push(select(&ValidatedStatement {
                        policy_id: &policy.policy_id,
                        revision: policy.revision,
                        schema: &source.schema,
                        rule,
                    }));
                }
            }
            selections.push(rows);
        }
        Ok(Self {
            generation: sources.generation,
            select,
            sources,
            selections,
        })
    }
}

pub(super) fn authorization_digest(policies: &[AuthorizationPolicy]) -> Result<String, PapError> {
    let bytes = serde_json::to_vec(policies).map_err(|_| PapError::Persistence)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) struct Sources {
    generation: u64,
    pub(super) policies: Arc<Vec<PreparedPolicy>>,
    parsed: Vec<Option<Arc<ValidatedPolicy>>>,
}

impl Sources {
    fn parse(snapshot: PolicySnapshot, other: Option<&Self>) -> Result<Self, PapError> {
        let parsed = snapshot
            .policies
            .iter()
            .map(|policy| {
                let PolicySource::Cedar(source) = &policy.template else {
                    return Ok(None);
                };
                if let Some(shared) = other.and_then(|shared| {
                    let i = shared
                        .policies
                        .binary_search_by(|p| p.policy_id.cmp(&policy.policy_id))
                        .ok()?;
                    (shared.policies[i].revision == policy.revision)
                        .then(|| shared.parsed[i].clone())
                        .flatten()
                }) {
                    return Ok(Some(shared));
                }
                ValidatedPolicy::parse(source)
                    .map(|p| Some(Arc::new(p)))
                    .map_err(|e| {
                        PapError::InvalidCedar(format!(
                            "{} revision {}: {}",
                            policy.policy_id,
                            policy.revision.get(),
                            e.diagnostic()
                        ))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            generation: snapshot.generation,
            policies: Arc::new(snapshot.policies),
            parsed,
        })
    }
}

type Slot = Mutex<Option<Arc<ConsumerPolicySet>>>;

#[derive(Default)]
pub(super) struct SelectionCache {
    // The two reads have different admission limits. They can share the parsed v3 policies.
    sources: Mutex<[Option<Arc<Sources>>; 2]>,
    sandbox: Slot,
    deployment_slots: Mutex<BTreeMap<DeploymentBackend, Arc<Slot>>>,
}

pub(super) type DeploymentViews = Vec<(DeploymentSelector, Arc<ConsumerPolicySet>)>;

impl SelectionCache {
    fn sources(
        &self,
        repo: &impl PapRepository,
        kind: PolicySnapshotKind,
    ) -> Result<Arc<Sources>, PapError> {
        let index = usize::from(kind == PolicySnapshotKind::Automatic);
        let mut slots = self.sources.lock().map_err(|_| PapError::Unavailable)?;
        let generation = repo.policy_generation()?;
        if let Some(sources) = slots[index].as_ref().filter(|s| s.generation == generation) {
            return Ok(sources.clone());
        }
        let snapshot = repo.policy_snapshot(kind)?;
        let sources = Arc::new(Sources::parse(snapshot, slots[1 - index].as_deref())?);
        slots[index] = Some(sources.clone());
        Ok(sources)
    }

    pub(super) fn sandbox(
        &self,
        repo: &impl PapRepository,
        select: Select,
    ) -> Result<Arc<ConsumerPolicySet>, PapError> {
        let sources = self.sources(repo, PolicySnapshotKind::AllCurrent)?;
        Self::select(&self.sandbox, sources, select, true)
    }

    pub(super) fn deployment(
        &self,
        repo: &impl PapRepository,
        selectors: &[DeploymentSelector],
    ) -> Result<(Arc<Vec<PreparedPolicy>>, DeploymentViews), PapError> {
        let sources = self.sources(repo, PolicySnapshotKind::Automatic)?;
        self.deployment_views(&sources, selectors)
    }

    pub(super) fn isolated_deployment(
        policies: Vec<PreparedPolicy>,
        selectors: &[DeploymentSelector],
    ) -> Result<(Arc<Vec<PreparedPolicy>>, DeploymentViews), PapError> {
        let snapshot = PolicySnapshot {
            generation: 0,
            policies,
        };
        Self::default().deployment_views(&Arc::new(Sources::parse(snapshot, None)?), selectors)
    }

    fn deployment_views(
        &self,
        sources: &Arc<Sources>,
        selectors: &[DeploymentSelector],
    ) -> Result<(Arc<Vec<PreparedPolicy>>, DeploymentViews), PapError> {
        let views = selectors
            .iter()
            .map(|selector| {
                let slot = self
                    .deployment_slots
                    .lock()
                    .map_err(|_| PapError::Unavailable)?
                    .entry(selector.backend)
                    .or_default()
                    .clone();
                Ok((
                    *selector,
                    Self::select(&slot, sources.clone(), selector.select, false)?,
                ))
            })
            .collect::<Result<_, PapError>>()?;
        Ok((sources.policies.clone(), views))
    }

    fn select(
        slot: &Slot,
        sources: Arc<Sources>,
        select: Select,
        sandbox: bool,
    ) -> Result<Arc<ConsumerPolicySet>, PapError> {
        let mut slot = slot.lock().map_err(|_| PapError::Unavailable)?;
        if let Some(current) = slot.as_ref().filter(|s| {
            s.generation == sources.generation && std::ptr::fn_addr_eq(s.select, select)
        }) {
            return Ok(current.clone());
        }
        let view = Arc::new(ConsumerPolicySet::build(sources, select, sandbox)?);
        if slot
            .as_ref()
            .is_none_or(|current| current.generation <= view.generation)
        {
            *slot = Some(view.clone());
        }
        Ok(view)
    }
}
