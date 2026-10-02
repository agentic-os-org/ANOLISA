//! Application service for normalized audit ingestion and risk correlation.

use std::sync::Arc;

use agentsight_enforcement_protocol::{
    CredentialExfiltrationPolicy, DestinationClass, PolicyDecision, PolicyMode, SecurityEvent,
    SecurityEventKind,
};
use std::collections::HashMap;
use std::collections::HashSet;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    AuditError, AuditEventFilter, AuditEventPage, AuditSessionPage, AuditStore, AuditSummary,
    ContainmentAction, RiskCase, RiskCaseDetail, RiskCaseStatus, RiskCaseSummary, RiskSeverity,
};

/// Failures returned by system-audit application operations.
#[derive(Debug, Error)]
pub enum AuditServiceError {
    /// Local persistence or query failed.
    #[error(transparent)]
    Store(#[from] AuditError),
    /// A decision referenced evidence that has not been persisted.
    #[error("audit decision references missing {kind} event {event_id}")]
    MissingEvidence {
        /// Expected evidence role.
        kind: &'static str,
        /// Missing stable event identifier.
        event_id: Uuid,
    },
}

/// Application boundary for system-audit operations.
pub struct AuditService {
    store: Arc<AuditStore>,
}

impl AuditService {
    /// Registers one immutable product-policy revision before enforcement.
    ///
    /// Reusing the same identity with byte-different normalized contents is
    /// rejected so evidence always resolves to one policy definition.
    ///
    /// # Errors
    ///
    /// Returns a serialization, persistence, or revision-conflict error.
    pub fn register_policy_revision(
        &self,
        policy: &CredentialExfiltrationPolicy,
        created_at_ns: u64,
    ) -> Result<(), AuditError> {
        let policy_json = serde_json::to_string(policy)?;
        self.store.register_policy_revision(
            &policy.policy_id,
            policy.revision,
            &policy_json,
            created_at_ns,
        )
    }

    /// Creates a service over one shared audit store.
    pub fn new(store: Arc<AuditStore>) -> Self {
        Self { store }
    }

    /// Returns the shared persistence boundary.
    pub fn store(&self) -> &Arc<AuditStore> {
        &self.store
    }

    /// Deletes terminal audit graphs older than the supplied cutoff.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence error when cleanup fails.
    pub fn purge_before(&self, cutoff_ns: u64) -> Result<u64, AuditError> {
        self.store.purge_before(cutoff_ns)
    }

    /// Returns aggregate event totals for the supplied filter.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence error when the query fails.
    pub fn summary(&self, filter: &AuditEventFilter) -> Result<AuditSummary, AuditError> {
        self.store.summary_filtered(filter)
    }

    /// Lists normalized events for API and embedding consumers.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence or decoding error when the query fails.
    pub fn events(&self, filter: &AuditEventFilter) -> Result<AuditEventPage, AuditError> {
        self.store.list_events(filter)
    }

    /// Groups normalized events by session.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence or stored-data error when the query fails.
    pub fn sessions(&self, filter: &AuditEventFilter) -> Result<AuditSessionPage, AuditError> {
        self.store.list_sessions(filter)
    }

    /// Returns the number of correlated risk cases.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence error when the count fails.
    pub fn case_count(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
        blocked: Option<bool>,
    ) -> Result<u64, AuditError> {
        self.store.case_count(agent_id, status, blocked)
    }

    /// Returns complete correlated-case totals for summary views.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence error when the aggregate query fails.
    pub fn case_summary(&self) -> Result<RiskCaseSummary, AuditError> {
        self.store.case_summary()
    }

    /// Lists correlated risk cases with bounded pagination.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence or stored-data error when the query fails.
    pub fn cases(
        &self,
        limit: usize,
        offset: i64,
        agent_id: Option<&str>,
        status: Option<&str>,
        blocked: Option<bool>,
    ) -> Result<Vec<RiskCase>, AuditError> {
        self.store
            .list_cases(limit, offset, agent_id, status, blocked)
    }

    /// Returns a mapping from (agent_id, policy_id) to the most recent case_id.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence or stored-data error when the query fails.
    pub fn case_index_by_agent_policy(
        &self,
    ) -> Result<HashMap<(String, String, String), Uuid>, AuditError> {
        self.store.case_index_by_agent_policy()
    }

    /// Returns a mapping from event_id to case_id using the precise
    /// risk_evidence_links table.
    pub fn case_ids_for_events(
        &self,
        event_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Uuid>, AuditError> {
        self.store.case_ids_for_events(event_ids)
    }

    /// Returns one risk case and its ordered immutable evidence.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::MissingCase`] when absent, or a typed persistence
    /// or stored-data error when the query fails.
    pub fn case(&self, case_id: Uuid) -> Result<RiskCaseDetail, AuditError> {
        self.store.case_detail(case_id)
    }

    /// Returns the latest containment action associated with a case.
    ///
    /// # Errors
    ///
    /// Returns a typed persistence or stored-data error when the query fails.
    pub fn latest_containment(
        &self,
        case_id: Uuid,
    ) -> Result<Option<ContainmentAction>, AuditError> {
        self.store.latest_containment_action(case_id)
    }

    /// Records a human review disposition without mutating evidence.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::MissingCase`] when absent, or a typed persistence
    /// error when the update fails.
    pub fn review(
        &self,
        case_id: Uuid,
        status: RiskCaseStatus,
        reviewed_at_ns: u64,
    ) -> Result<RiskCase, AuditError> {
        self.store.review_case(case_id, status, reviewed_at_ns)
    }

    /// Persists one normalized event and correlates policy decisions into cases.
    ///
    /// # Errors
    ///
    /// Returns a typed storage or missing-evidence error.
    pub fn ingest(&self, event: SecurityEvent) -> Result<(), AuditServiceError> {
        self.store.insert_event(&event)?;
        let SecurityEventKind::PolicyDecision(decision) = &event.kind else {
            return Ok(());
        };
        let containment_case = self
            .store
            .case_id_for_containment_binding(event.identity.binding_id)?;
        if decision.mode == PolicyMode::Observe && containment_case.is_none() {
            return Ok(());
        }

        let source = self.store.event(decision.source_event_id)?.ok_or(
            AuditServiceError::MissingEvidence {
                kind: "source",
                event_id: decision.source_event_id,
            },
        )?;
        let sink = self.store.event(decision.sink_event_id)?.ok_or(
            AuditServiceError::MissingEvidence {
                kind: "sink",
                event_id: decision.sink_event_id,
            },
        )?;
        let mut transitions = self
            .store
            .list_events(&AuditEventFilter {
                start_ns: Some(source.occurred_at_ns),
                end_ns: Some(event.occurred_at_ns),
                event_type: Some("taint_transition".into()),
                policy_id: Some(decision.policy_id.clone()),
                binding_id: Some(event.identity.binding_id),
                limit: 1_000,
                ..AuditEventFilter::default()
            })?
            .items;
        transitions.sort_by_key(|item| (item.occurred_at_ns, item.event_id));
        let transitions = transitions_on_process_chain(&source, &sink, transitions);

        let mut evidence_ids = Vec::with_capacity(transitions.len().saturating_add(3));
        evidence_ids.push(source.event_id);
        evidence_ids.extend(transitions.into_iter().map(|item| item.event_id));
        evidence_ids.push(sink.event_id);
        evidence_ids.push(event.event_id);
        if let Some(case_id) = containment_case {
            self.store.append_containment_evidence(
                case_id,
                event.identity.binding_id,
                &evidence_ids,
                decision.risk_score,
                decision.blocked,
                event.occurred_at_ns,
            )?;
            return Ok(());
        }

        let destination_class = match &sink.kind {
            SecurityEventKind::NetworkAction(network) => network.destination_class,
            _ => DestinationClass::Unknown,
        };
        let severity = if decision.blocked {
            RiskSeverity::Critical
        } else if destination_class == DestinationClass::Trusted {
            RiskSeverity::Medium
        } else {
            RiskSeverity::High
        };
        let case = RiskCase {
            case_id: event.event_id,
            correlation_key: risk_correlation_key(&event, decision, &source, &sink),
            policy_id: decision.policy_id.clone(),
            policy_revision: decision.policy_revision,
            agent_id: event.identity.agent_id.clone(),
            session_id: event.identity.session_id.clone(),
            severity,
            risk_score: decision.risk_score,
            status: RiskCaseStatus::Open,
            blocked: decision.blocked,
            opened_at_ns: source.occurred_at_ns,
            updated_at_ns: event.occurred_at_ns,
            summary: decision.reason.clone(),
        };
        self.store.upsert_case(&case, &evidence_ids)?;
        Ok(())
    }
}

fn transitions_on_process_chain(
    source: &SecurityEvent,
    sink: &SecurityEvent,
    transitions: Vec<SecurityEvent>,
) -> Vec<SecurityEvent> {
    let source_identity = (source.identity.pid, source.identity.process_start_time);
    let sink_identity = (sink.identity.pid, sink.identity.process_start_time);
    let edges = transitions
        .iter()
        .filter_map(|event| match &event.kind {
            SecurityEventKind::TaintTransition(transition) => Some((
                (transition.source_pid, transition.source_process_start_time),
                (transition.target_pid, transition.target_process_start_time),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut reachable_from_source = HashSet::from([source_identity]);
    let mut changed = true;
    while changed {
        changed = false;
        for (from, to) in &edges {
            if reachable_from_source.contains(from) {
                changed |= reachable_from_source.insert(*to);
            }
        }
    }
    if !reachable_from_source.contains(&sink_identity) {
        return Vec::new();
    }

    let mut reaches_sink = HashSet::from([sink_identity]);
    changed = true;
    while changed {
        changed = false;
        for (from, to) in edges.iter().rev() {
            if reaches_sink.contains(to) {
                changed |= reaches_sink.insert(*from);
            }
        }
    }

    transitions
        .into_iter()
        .filter(|event| match &event.kind {
            SecurityEventKind::TaintTransition(transition) => {
                let from = (transition.source_pid, transition.source_process_start_time);
                let to = (transition.target_pid, transition.target_process_start_time);
                reachable_from_source.contains(&from) && reaches_sink.contains(&to)
            }
            _ => false,
        })
        .collect()
}

fn risk_correlation_key(
    decision_event: &SecurityEvent,
    decision: &PolicyDecision,
    source: &SecurityEvent,
    sink: &SecurityEvent,
) -> String {
    const BURST_WINDOW_NS: u64 = 5_000_000_000;

    let source_resource = match &source.kind {
        SecurityEventKind::FileAction(action) => action.path.as_str(),
        _ => "unknown-source",
    };
    let burst = source.occurred_at_ns / BURST_WINDOW_NS;
    format!(
        "burst-v1:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        decision_event.identity.binding_id,
        decision.policy_id,
        decision.policy_revision,
        source.identity.pid,
        source.identity.process_start_time,
        sink.identity.pid,
        sink.identity.process_start_time,
        burst,
        source_resource.len(),
        source_resource
    )
}

#[cfg(test)]
mod tests {
    use agentsight_enforcement_protocol::{
        DestinationClass, EventIdentity, FileAction, NetworkAction, NetworkDirection,
        TaintTransition, TaintTransitionKind,
    };

    use super::*;

    fn identity(pid: i32, start: u64) -> EventIdentity {
        EventIdentity {
            binding_id: Uuid::nil(),
            agent_id: "agent".into(),
            agent_name: None,
            session_id: Some("session".into()),
            conversation_id: None,
            tool_call_id: None,
            pid,
            process_start_time: start,
            ppid: None,
            cgroup_id: None,
            protocol_version: 1,
            enforcer_version: "test".into(),
            actplane_revision: "test".into(),
        }
    }

    fn event(identity: EventIdentity, kind: SecurityEventKind, time: u64) -> SecurityEvent {
        SecurityEvent {
            event_id: Uuid::new_v4(),
            occurred_at_ns: time,
            observed_at_ns: time,
            identity,
            kind,
        }
    }

    fn transition(from: (i32, u64), to: (i32, u64), time: u64) -> SecurityEvent {
        event(
            identity(to.0, to.1),
            SecurityEventKind::TaintTransition(TaintTransition {
                policy_id: "policy".into(),
                policy_revision: 1,
                label: "SENSITIVE".into(),
                transition: TaintTransitionKind::Inherit,
                source_pid: from.0,
                source_process_start_time: from.1,
                target_pid: to.0,
                target_process_start_time: to.1,
                reason: "inherit".into(),
            }),
            time,
        )
    }

    #[test]
    fn process_chain_excludes_concurrent_unrelated_taint_transitions() {
        let source = event(
            identity(10, 100),
            SecurityEventKind::FileAction(FileAction {
                policy_id: "policy".into(),
                policy_revision: 1,
                operation: "read".into(),
                path: "/secret".into(),
                resource_class: "credential".into(),
                succeeded: true,
                errno: None,
                rule_id: None,
            }),
            1,
        );
        let sink = event(
            identity(12, 120),
            SecurityEventKind::NetworkAction(NetworkAction {
                policy_id: "policy".into(),
                policy_revision: 1,
                direction: NetworkDirection::Outbound,
                destination: "203.0.113.10:443".into(),
                destination_class: DestinationClass::Public,
                protocol: "tcp".into(),
                succeeded: true,
                errno: None,
                rule_id: None,
            }),
            4,
        );
        let first = transition((10, 100), (11, 110), 2);
        let second = transition((11, 110), (12, 120), 3);
        let unrelated = transition((20, 200), (21, 210), 3);

        let selected = transitions_on_process_chain(
            &source,
            &sink,
            vec![first.clone(), unrelated, second.clone()],
        );
        assert_eq!(
            selected
                .iter()
                .map(|item| item.event_id)
                .collect::<Vec<_>>(),
            vec![first.event_id, second.event_id]
        );
    }
}
