//! Deterministic backend for protocol and service tests.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    DestinationClass, Effect, EnforcementCapabilities, EventIdentity, FileAction, HealthStatus,
    NetworkAction, NetworkDirection, PolicyDecision, PolicyMode, ReplaceFailureCode,
    ReplaceOutcome, ReplacePolicy, ReplaceValidationError, ReplacementPolicy, SecurityEvent,
    SecurityEventKind, TaintTransition, TaintTransitionKind, ViolationEvent,
    classify_public_ipv4_destination,
};
use uuid::Uuid;

use crate::event_hub::SecurityEventHub;
use crate::{BackendError, EnforcementBackend, EventHub, SubscriberClass};

/// In-memory single-binding backend that performs no kernel operations.
pub struct MockBackend {
    bindings: Mutex<HashMap<Uuid, Binding>>,
    credential_policies: Mutex<HashMap<Uuid, CredentialExfiltrationPolicy>>,
    events: EventHub,
    security_events: SecurityEventHub,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    /// Creates an empty backend.
    pub fn new() -> Self {
        Self {
            bindings: Mutex::new(HashMap::new()),
            credential_policies: Mutex::new(HashMap::new()),
            events: EventHub::default(),
            security_events: SecurityEventHub::default(),
        }
    }

    fn apply_policy(
        &self,
        request: ApplyPolicy,
        credential_policy: Option<CredentialExfiltrationPolicy>,
    ) -> Result<Binding, BackendError> {
        let mut bindings = self.bindings();
        let mut credential_policies = self.credential_policies();
        if let Some(existing) = bindings.get(&request.binding_id) {
            return if existing.request == request
                && credential_policies.get(&request.binding_id) == credential_policy.as_ref()
            {
                Ok(existing.clone())
            } else {
                Err(BackendError::BindingConflict(request.binding_id))
            };
        }
        if !bindings.is_empty() {
            return Err(BackendError::BindingConflict(request.binding_id));
        }

        let binding = Binding {
            request,
            state: BindingState::Enforced,
            message: Some("mock acknowledgement; no kernel policy attached".into()),
            domain_id: Some(1),
        };
        if let Some(policy) = credential_policy {
            credential_policies.insert(binding.request.binding_id, policy);
        }
        bindings.insert(binding.request.binding_id, binding.clone());
        Ok(binding)
    }

    #[cfg(test)]
    fn with_event_capacity(capacity: usize) -> Self {
        Self {
            bindings: Mutex::new(HashMap::new()),
            credential_policies: Mutex::new(HashMap::new()),
            events: EventHub::new(capacity),
            security_events: SecurityEventHub::new(capacity),
        }
    }

    /// Injects a violation for an active binding.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::MissingBinding`] when the event references an
    /// unknown binding.
    pub fn publish_violation(&self, event: ViolationEvent) -> Result<(), BackendError> {
        if !self.bindings().contains_key(&event.binding_id) {
            return Err(BackendError::MissingBinding(event.binding_id));
        }
        self.events.publish(event);
        Ok(())
    }

    /// Emits a deterministic source, taint, sink, and decision chain.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::MissingBinding`] when `binding_id` is unknown.
    pub fn emit_credential_exfiltration(
        &self,
        binding_id: Uuid,
        source_path: &str,
        destination: &str,
    ) -> Result<(), BackendError> {
        self.emit_credential_exfiltration_after(
            binding_id,
            source_path,
            destination,
            Duration::ZERO,
        )
    }

    /// Emits a deterministic chain after a simulated delay from source read.
    ///
    /// Product policies suppress trusted, non-public, and expired sinks. This
    /// keeps the mock faithful to the product contract even when the pinned
    /// ActPlane backend must reject an unrepresentable enforcement policy.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::MissingBinding`] when `binding_id` is unknown.
    pub fn emit_credential_exfiltration_after(
        &self,
        binding_id: Uuid,
        source_path: &str,
        destination: &str,
        elapsed_since_source: Duration,
    ) -> Result<(), BackendError> {
        let binding = self
            .bindings()
            .get(&binding_id)
            .cloned()
            .ok_or(BackendError::MissingBinding(binding_id))?;
        let revision = binding.request.policy_revision.parse().unwrap_or(1);
        let product_policy = self.credential_policies().get(&binding_id).cloned();
        let mode = product_policy.as_ref().map_or_else(
            || mock_policy_mode(&binding.request.policy_dsl),
            |policy| policy.mode,
        );
        let destination_class = classify_destination(destination);
        if let Some(policy) = &product_policy
            && (elapsed_since_source >= Duration::from_secs(policy.taint_ttl_secs)
                || policy
                    .trusted_endpoints
                    .iter()
                    .any(|trusted| trusted == destination)
                || destination_class != DestinationClass::Public)
        {
            return Ok(());
        }
        let identity = event_identity(&binding);
        let base_time = unix_epoch_ns();
        let source_event_id = Uuid::new_v4();
        let sink_event_id = Uuid::new_v4();

        let events = [
            SecurityEvent {
                event_id: source_event_id,
                occurred_at_ns: base_time,
                observed_at_ns: base_time,
                identity: identity.clone(),
                kind: SecurityEventKind::FileAction(FileAction {
                    policy_id: binding.request.policy_id.clone(),
                    policy_revision: revision,
                    operation: "read".into(),
                    path: redact_home_path(source_path),
                    resource_class: "credential".into(),
                    succeeded: true,
                    errno: None,
                    rule_id: Some("credential-source".into()),
                }),
            },
            SecurityEvent {
                event_id: Uuid::new_v4(),
                occurred_at_ns: base_time.saturating_add(1),
                observed_at_ns: base_time.saturating_add(1),
                identity: identity.clone(),
                kind: SecurityEventKind::TaintTransition(TaintTransition {
                    policy_id: binding.request.policy_id.clone(),
                    policy_revision: revision,
                    label: "credential".into(),
                    transition: TaintTransitionKind::Add,
                    source_pid: binding.request.root_pid,
                    source_process_start_time: binding.request.process_start_time,
                    target_pid: binding.request.root_pid,
                    target_process_start_time: binding.request.process_start_time,
                    reason: "sensitive credential source read".into(),
                }),
            },
            SecurityEvent {
                event_id: sink_event_id,
                occurred_at_ns: base_time.saturating_add(2),
                observed_at_ns: base_time.saturating_add(2),
                identity: identity.clone(),
                kind: SecurityEventKind::NetworkAction(NetworkAction {
                    policy_id: binding.request.policy_id.clone(),
                    policy_revision: revision,
                    direction: NetworkDirection::Outbound,
                    destination: destination.into(),
                    destination_class,
                    protocol: "tcp".into(),
                    succeeded: mode != PolicyMode::Enforce,
                    errno: (mode == PolicyMode::Enforce).then_some(libc::EPERM),
                    rule_id: Some("credential-public-sink".into()),
                }),
            },
            SecurityEvent {
                event_id: Uuid::new_v4(),
                occurred_at_ns: base_time.saturating_add(3),
                observed_at_ns: base_time.saturating_add(3),
                identity,
                kind: SecurityEventKind::PolicyDecision(PolicyDecision {
                    policy_id: binding.request.policy_id,
                    policy_revision: revision,
                    source_event_id,
                    sink_event_id,
                    mode,
                    requested_effect: if mode == PolicyMode::Enforce {
                        Effect::Block
                    } else {
                        Effect::Notify
                    },
                    blocked: mode == PolicyMode::Enforce,
                    killed: false,
                    errno: (mode == PolicyMode::Enforce).then_some(libc::EPERM),
                    risk_score: 85,
                    reason: "credential taint reached unknown public endpoint".into(),
                }),
            },
        ];
        for event in events {
            self.security_events.publish(event);
        }
        Ok(())
    }

    fn bindings(&self) -> MutexGuard<'_, HashMap<Uuid, Binding>> {
        self.bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn credential_policies(&self) -> MutexGuard<'_, HashMap<Uuid, CredentialExfiltrationPolicy>> {
        self.credential_policies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl EnforcementBackend for MockBackend {
    fn health(&self) -> Result<HealthStatus, BackendError> {
        let health = self.events.reflect_delivery_loss(HealthStatus {
            ready: true,
            backend: "mock".into(),
            capabilities: EnforcementCapabilities::mock_development(),
            message: Some("mock backend does not enforce kernel operations".into()),
        });
        Ok(self.security_events.reflect_delivery_loss(health))
    }

    fn apply(&self, request: ApplyPolicy) -> Result<Binding, BackendError> {
        self.apply_policy(request, None)
    }

    fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
    ) -> Result<Binding, BackendError> {
        let (request, policy) = mock_credential_request(request)?;
        self.apply_policy(request, Some(policy))
    }

    fn replace(&self, request: ReplacePolicy) -> Result<ReplaceOutcome, BackendError> {
        let mut bindings = self.bindings();
        let mut credential_policies = self.credential_policies();
        let actual = bindings.values().next().cloned();
        if let Err(error) = request.validate() {
            let exact_source = actual.as_ref() == Some(&request.expected);
            return Ok(if exact_source {
                ReplaceOutcome::SourceRetained {
                    binding: request.expected,
                    code: mock_validation_failure_code(&error),
                }
            } else {
                ReplaceOutcome::Conflict {
                    code: ReplaceFailureCode::BindingConflict,
                }
            });
        }
        let (target_request, target_policy) = match request.replacement.clone() {
            ReplacementPolicy::Generic(target) => (target, None),
            ReplacementPolicy::Credential(target) => {
                let (target, policy) = mock_credential_request(target)?;
                (target, Some(policy))
            }
        };
        let target = Binding {
            request: target_request,
            state: BindingState::Enforced,
            message: Some("mock acknowledgement; no kernel policy attached".into()),
            domain_id: actual
                .as_ref()
                .and_then(|binding| binding.domain_id)
                .or(Some(1)),
        };
        if let Some(actual) = actual.as_ref()
            && actual.request == target.request
        {
            return if credential_policies.get(&target.request.binding_id) == target_policy.as_ref()
                && request.validate_acknowledgement(actual).is_ok()
            {
                Ok(ReplaceOutcome::Applied(actual.clone()))
            } else {
                Ok(ReplaceOutcome::Conflict {
                    code: ReplaceFailureCode::BindingConflict,
                })
            };
        }
        match actual {
            Some(actual) if actual == request.expected => {
                if credential_policies.get(&actual.request.binding_id)
                    != request
                        .source
                        .credential_snapshot()
                        .map(|snapshot| &snapshot.policy)
                    || request.validate_acknowledgement(&target).is_err()
                {
                    return Ok(ReplaceOutcome::SourceRetained {
                        binding: actual,
                        code: ReplaceFailureCode::BindingConflict,
                    });
                }
                bindings.clear();
                credential_policies.clear();
                if let Some(policy) = target_policy {
                    credential_policies.insert(target.request.binding_id, policy);
                }
                bindings.insert(target.request.binding_id, target.clone());
                Ok(ReplaceOutcome::Applied(target))
            }
            None => Ok(ReplaceOutcome::Conflict {
                code: ReplaceFailureCode::BindingConflict,
            }),
            Some(_) => Ok(ReplaceOutcome::Conflict {
                code: ReplaceFailureCode::BindingConflict,
            }),
        }
    }

    fn detach(&self, binding_id: Uuid) -> Result<(), BackendError> {
        if self.bindings().remove(&binding_id).is_some() {
            self.credential_policies().remove(&binding_id);
            Ok(())
        } else {
            Err(BackendError::MissingBinding(binding_id))
        }
    }

    fn bindings(&self) -> Result<Vec<Binding>, BackendError> {
        let mut bindings: Vec<_> = self.bindings().values().cloned().collect();
        bindings.sort_by_key(|binding| binding.request.binding_id);
        Ok(bindings)
    }

    fn subscribe(&self, id: Uuid, class: SubscriberClass) -> Receiver<ViolationEvent> {
        self.events.subscribe(id, class)
    }

    fn unsubscribe(&self, id: Uuid) {
        self.events.unsubscribe(id);
    }

    fn record_required_delivery_loss(&self, count: u64) {
        self.events.record_required_delivery_loss(count);
    }

    fn subscribe_security_events(&self) -> Receiver<SecurityEvent> {
        self.security_events.subscribe()
    }

    fn record_security_delivery_loss(&self, count: u64) {
        self.security_events.record_delivery_loss(count);
    }
}

fn mock_validation_failure_code(error: &ReplaceValidationError) -> ReplaceFailureCode {
    match error {
        ReplaceValidationError::CredentialPolicy(_)
        | ReplaceValidationError::SourcePolicySnapshot(_) => ReplaceFailureCode::CompileFailure,
        ReplaceValidationError::SameBindingId
        | ReplaceValidationError::SourceNotEnforced
        | ReplaceValidationError::SourceDomainMissing
        | ReplaceValidationError::AgentMismatch
        | ReplaceValidationError::SessionMismatch
        | ReplaceValidationError::SourcePolicyMismatch
        | ReplaceValidationError::TargetAcknowledgementMismatch
        | ReplaceValidationError::RuntimeDomainMismatch => ReplaceFailureCode::BindingConflict,
    }
}

fn mock_credential_request(
    request: ApplyCredentialPolicy,
) -> Result<(ApplyPolicy, CredentialExfiltrationPolicy), BackendError> {
    request
        .policy
        .validate()
        .map_err(|error| BackendError::CompileFailure(error.to_string()))?;
    let action = if request.policy.mode == PolicyMode::Enforce {
        "block"
    } else {
        "notify"
    };
    let mut policy_dsl = format!(
        "mode {} ttl {}\nsource AGENT = exec \"**\"\n",
        policy_mode_name(request.policy.mode),
        request.policy.taint_ttl_secs
    );
    for source in &request.policy.source_patterns {
        policy_dsl.push_str(&format!(
            "source {} = file \"{}\"\n",
            request.policy.taint_label, source
        ));
    }
    policy_dsl.push_str(&format!(
        "rule agentsight-credential-exfiltration:\n  {action} connect endpoint \"*\" if {}\n",
        request.policy.taint_label,
    ));
    let policy = request.policy;
    Ok((
        ApplyPolicy {
            binding_id: request.binding_id,
            agent_id: request.agent_id,
            session_id: request.session_id,
            root_pid: request.root_pid,
            process_start_time: request.process_start_time,
            policy_id: policy.policy_id.clone(),
            policy_revision: policy.revision.to_string(),
            policy_dsl,
            policy_mode: Some(policy.mode),
        },
        policy,
    ))
}

fn event_identity(binding: &Binding) -> EventIdentity {
    // The real ActPlane path captures a genuine parent PID for the traced
    // process (see `actplane::event_identity`). The mock enforcer has no live
    // eBPF probe, so it models the common provenance where the traced Agent
    // process was launched under a parent that it inherited the credential
    // taint from. Emitting a synthetic parent PID (one below the tracked root)
    // lets the dashboard's process tree still render an "entry -> child" chain
    // on hosts without eBPF (e.g. macOS) instead of collapsing to a single
    // node. The parent has no evidence of its own and therefore surfaces as
    // the process-tree entry.
    let pid = binding.request.root_pid;
    let ppid = pid.checked_sub(1).filter(|value| *value > 0);
    EventIdentity {
        binding_id: binding.request.binding_id,
        agent_id: binding.request.agent_id.clone(),
        agent_name: Some("mock-agent".into()),
        session_id: binding.request.session_id.clone(),
        conversation_id: None,
        tool_call_id: None,
        pid,
        process_start_time: binding.request.process_start_time,
        ppid,
        cgroup_id: None,
        protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
        enforcer_version: env!("CARGO_PKG_VERSION").into(),
        actplane_revision: "mock".into(),
    }
}

fn mock_policy_mode(policy_dsl: &str) -> PolicyMode {
    if policy_dsl.split_whitespace().any(|word| word == "enforce") {
        PolicyMode::Enforce
    } else if policy_dsl.split_whitespace().any(|word| word == "observe") {
        PolicyMode::Observe
    } else {
        PolicyMode::Audit
    }
}

fn policy_mode_name(mode: PolicyMode) -> &'static str {
    match mode {
        PolicyMode::Observe => "observe",
        PolicyMode::Audit => "audit",
        PolicyMode::Enforce => "enforce",
    }
}

fn classify_destination(destination: &str) -> DestinationClass {
    classify_public_ipv4_destination(destination)
}

fn redact_home_path(path: &str) -> String {
    if let Some(relative) = path.strip_prefix("/root/") {
        return format!("~/{relative}");
    }
    let Some(home_relative) = path.strip_prefix("/home/") else {
        return path.into();
    };
    let Some((_, relative)) = home_relative.split_once('/') else {
        return path.into();
    };
    format!("~/{relative}")
}

fn unix_epoch_ns() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use agentsight_enforcement_protocol::{
        Effect, ReplaceFailureCode, ReplaceOutcome, ReplacePolicy, ReplacementPolicy,
        ReplacementSource,
    };

    use super::*;

    fn request() -> ApplyPolicy {
        ApplyPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "mock-health-test".into(),
            session_id: None,
            root_pid: 42,
            process_start_time: 99,
            policy_id: "policy".into(),
            policy_revision: "revision".into(),
            policy_dsl: "label AGENT".into(),
            policy_mode: None,
        }
    }

    fn violation(binding_id: Uuid) -> ViolationEvent {
        ViolationEvent {
            event_id: Uuid::new_v4(),
            binding_id,
            agent_id: "mock-health-test".into(),
            session_id: None,
            policy_id: "policy".into(),
            policy_revision: "revision".into(),
            pid: 42,
            ppid: Some(1),
            process_start_time: 99,
            operation: "open".into(),
            target: "/tmp/secret".into(),
            effect: Effect::Block,
            blocked: true,
            killed: false,
            rule_id: None,
            reason: None,
            occurred_at_ns: 100,
            observed_at_ns: 101,
            actplane_revision: "mock".into(),
        }
    }

    fn replacement(expected: Binding, target: ApplyPolicy) -> ReplacePolicy {
        ReplacePolicy {
            expected,
            source: ReplacementSource::Generic,
            replacement: ReplacementPolicy::Generic(target),
        }
    }

    #[test]
    fn replace_moves_runtime_ownership_without_an_empty_snapshot() {
        let backend = MockBackend::new();
        let source = backend.apply(request()).expect("source should apply");
        let target = request();

        let outcome = backend
            .replace(replacement(source.clone(), target.clone()))
            .expect("replace should complete");

        let ReplaceOutcome::Applied(applied) = outcome else {
            panic!("target should own the runtime");
        };
        assert_eq!(applied.request, target);
        assert_eq!(
            EnforcementBackend::bindings(&backend).expect("bindings should load"),
            vec![applied]
        );
        assert_ne!(source.request.binding_id, target.binding_id);
    }

    #[test]
    fn replace_retargets_runtime_ownership_to_an_alternate_process() {
        let backend = MockBackend::new();
        let source = backend.apply(request()).expect("source should apply");
        let mut target = request();
        target.root_pid = 77;
        target.process_start_time = 123;

        let outcome = backend
            .replace(replacement(source, target.clone()))
            .expect("alternate process replacement should complete");

        let ReplaceOutcome::Applied(applied) = outcome else {
            panic!("alternate process should own the runtime");
        };
        assert_eq!(applied.request, target);
    }

    #[test]
    fn replace_is_idempotent_when_target_already_owns_runtime() {
        let backend = MockBackend::new();
        let source = Binding {
            request: request(),
            state: BindingState::Enforced,
            message: None,
            domain_id: Some(1),
        };
        let target = request();
        let active = backend.apply(target.clone()).expect("target should apply");

        let outcome = backend
            .replace(replacement(source, target))
            .expect("repeat should complete");

        assert_eq!(outcome, ReplaceOutcome::Applied(active));
    }

    #[test]
    fn replace_with_the_exact_active_binding_id_retains_the_source() {
        let backend = MockBackend::new();
        let source = backend.apply(request()).expect("source should apply");

        let outcome = backend
            .replace(replacement(source.clone(), source.request.clone()))
            .expect("same-owner replacement should be typed");

        assert_eq!(
            outcome,
            ReplaceOutcome::SourceRetained {
                binding: source.clone(),
                code: ReplaceFailureCode::BindingConflict,
            }
        );
        assert_eq!(
            EnforcementBackend::bindings(&backend).expect("bindings should load"),
            vec![source]
        );
    }

    #[test]
    fn queued_security_events_keep_source_provenance_after_mock_handoff() {
        let backend = MockBackend::new();
        let source_request = request();
        let source_policy = CredentialExfiltrationPolicy {
            policy_id: source_request.policy_id.clone(),
            revision: 1,
            source_patterns: vec!["/tmp/credential".into()],
            trusted_endpoints: Vec::new(),
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: 900,
            destination_scope: agentsight_enforcement_protocol::DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        };
        let source = backend
            .apply_credential_policy(ApplyCredentialPolicy {
                binding_id: source_request.binding_id,
                agent_id: source_request.agent_id.clone(),
                session_id: source_request.session_id.clone(),
                root_pid: source_request.root_pid,
                process_start_time: source_request.process_start_time,
                policy: source_policy.clone(),
            })
            .expect("source should apply");
        let events = backend.subscribe_security_events();
        backend
            .emit_credential_exfiltration(source.request.binding_id, "/tmp/credential", "8.8.8.8")
            .expect("source events should queue");
        let target = request();

        let outcome = backend
            .replace(ReplacePolicy {
                expected: source.clone(),
                source: ReplacementSource::Credential(
                    agentsight_enforcement_protocol::CredentialPolicySnapshot::capture(
                        source_policy,
                    )
                    .expect("source policy should be valid"),
                ),
                replacement: ReplacementPolicy::Generic(target),
            })
            .expect("handoff should complete");

        assert!(matches!(outcome, ReplaceOutcome::Applied(_)));
        let queued: Vec<_> = events.try_iter().collect();
        assert!(!queued.is_empty());
        assert!(queued.iter().all(|event| {
            event.identity.binding_id == source.request.binding_id
                && event.identity.agent_id == source.request.agent_id
                && event.identity.session_id == source.request.session_id
        }));
    }

    #[test]
    fn reverse_restores_source_policy_and_normalized_event_provenance() {
        let backend = MockBackend::new();
        let source_request = request();
        let source_policy = CredentialExfiltrationPolicy {
            policy_id: source_request.policy_id.clone(),
            revision: 1,
            source_patterns: vec!["/tmp/credential".into()],
            trusted_endpoints: Vec::new(),
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: 300,
            destination_scope: agentsight_enforcement_protocol::DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        };
        let source_apply = ApplyCredentialPolicy {
            binding_id: source_request.binding_id,
            agent_id: source_request.agent_id.clone(),
            session_id: source_request.session_id.clone(),
            root_pid: source_request.root_pid,
            process_start_time: source_request.process_start_time,
            policy: source_policy.clone(),
        };
        let source = backend
            .apply_credential_policy(source_apply)
            .expect("source should apply");
        let mut target_policy = source_policy.clone();
        target_policy.mode = PolicyMode::Enforce;
        let target_apply = ApplyCredentialPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: source.request.agent_id.clone(),
            session_id: source.request.session_id.clone(),
            root_pid: source.request.root_pid,
            process_start_time: source.request.process_start_time,
            policy: target_policy,
        };
        let forward = ReplacePolicy {
            expected: source.clone(),
            source: ReplacementSource::Credential(
                agentsight_enforcement_protocol::CredentialPolicySnapshot::capture(source_policy)
                    .expect("source policy should be valid"),
            ),
            replacement: ReplacementPolicy::Credential(target_apply),
        };
        let ReplaceOutcome::Applied(target) = backend
            .replace(forward.clone())
            .expect("forward handoff should apply")
        else {
            panic!("forward handoff should apply target");
        };

        let reverse = forward.reverse(target);
        let ReplaceOutcome::Applied(restored) = backend
            .replace(reverse)
            .expect("reverse handoff should restore source")
        else {
            panic!("reverse handoff should apply source");
        };
        let events = backend.subscribe_security_events();
        backend
            .emit_credential_exfiltration(restored.request.binding_id, "/tmp/credential", "8.8.8.8")
            .expect("restored source should normalize events");
        backend
            .emit_credential_exfiltration_after(
                restored.request.binding_id,
                "/tmp/credential",
                "8.8.8.8",
                Duration::from_secs(300),
            )
            .expect("restored source TTL should suppress expired taint");

        let normalized: Vec<_> = events.try_iter().collect();
        assert!(!normalized.is_empty());
        assert!(normalized.iter().all(|event| {
            event.identity.binding_id == source.request.binding_id
                && event.identity.agent_id == source.request.agent_id
                && event.identity.session_id == source.request.session_id
        }));
        assert!(normalized.iter().any(|event| matches!(
            &event.kind,
            SecurityEventKind::PolicyDecision(decision) if decision.mode == PolicyMode::Audit
        )));
        assert_eq!(normalized.len(), 4);
    }

    #[test]
    fn replace_never_detaches_a_third_party_binding() {
        let backend = MockBackend::new();
        let third_party = backend.apply(request()).expect("third party should apply");
        let expected = Binding {
            request: request(),
            state: BindingState::Enforced,
            message: None,
            domain_id: Some(7),
        };

        let outcome = backend
            .replace(replacement(expected, request()))
            .expect("conflict should be typed");

        assert_eq!(
            outcome,
            ReplaceOutcome::Conflict {
                code: ReplaceFailureCode::BindingConflict,
            }
        );
        assert_eq!(
            EnforcementBackend::bindings(&backend).expect("bindings should load"),
            vec![third_party]
        );
    }

    #[test]
    fn health_is_not_ready_after_the_violation_queue_overflows() {
        let backend = MockBackend::with_event_capacity(1);
        let binding = backend.apply(request()).expect("binding should apply");
        let _subscriber = backend.subscribe(Uuid::new_v4(), SubscriberClass::Required);

        for _ in 0..2 {
            backend
                .publish_violation(violation(binding.request.binding_id))
                .expect("active binding should publish");
        }

        let health = backend.health().expect("mock health should load");
        assert!(!health.ready);
        assert_eq!(
            health.message.as_deref(),
            Some(
                "mock backend does not enforce kernel operations; violation event delivery loss: dropped_events=1"
            )
        );
    }
}
