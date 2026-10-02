//! Stable product-level security events and progressive enforcement policies.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::Effect;

/// Maximum number of patterns accepted in one policy list.
pub const MAX_POLICY_PATTERNS: usize = 1_024;

/// One immutable security fact, state transition, or policy decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityEvent {
    /// Stable identifier used for idempotent ingestion.
    pub event_id: Uuid,
    /// Kernel occurrence time as Unix epoch nanoseconds.
    pub occurred_at_ns: u64,
    /// AgentSight observation time as Unix epoch nanoseconds.
    pub observed_at_ns: u64,
    /// Agent, session, and process identity shared by all event kinds.
    pub identity: EventIdentity,
    /// Explicitly tagged event payload.
    #[serde(flatten)]
    pub kind: SecurityEventKind,
}

impl SecurityEvent {
    /// Builds a policy-decision event with a fresh ID and observation time.
    pub fn policy_decision(identity: EventIdentity, decision: PolicyDecision) -> Self {
        let now = unix_epoch_ns();
        Self {
            event_id: Uuid::new_v4(),
            occurred_at_ns: now,
            observed_at_ns: now,
            identity,
            kind: SecurityEventKind::PolicyDecision(decision),
        }
    }
}

/// Explicit event discriminator and its typed payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "event", rename_all = "snake_case")]
pub enum SecurityEventKind {
    /// A file operation observed without collecting file contents.
    FileAction(FileAction),
    /// A process-label lifecycle or propagation transition.
    TaintTransition(TaintTransition),
    /// A network operation and destination classification.
    NetworkAction(NetworkAction),
    /// A policy match with requested and observed outcomes.
    PolicyDecision(PolicyDecision),
    /// Enforcement availability, reconciliation, or evidence-loss state.
    EnforcementState(EnforcementStateEvent),
}

/// Identity used to associate one event with an Agent process tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventIdentity {
    /// Stable policy-binding identifier.
    pub binding_id: Uuid,
    /// Product-level Agent identifier.
    pub agent_id: String,
    /// Optional display name for the Agent implementation.
    pub agent_name: Option<String>,
    /// Optional AgentSight session identifier.
    pub session_id: Option<String>,
    /// Optional conversation identifier supplied by the Agent adapter.
    pub conversation_id: Option<String>,
    /// Optional tool-call identifier supplied by the Agent adapter.
    pub tool_call_id: Option<String>,
    /// Process identifier observed for the event.
    pub pid: i32,
    /// Linux process start time used with PID to reject reuse.
    pub process_start_time: u64,
    /// Parent process identifier when known.
    pub ppid: Option<i32>,
    /// Kernel cgroup identifier when known.
    pub cgroup_id: Option<u64>,
    /// Enforcer wire-protocol version that produced the event.
    pub protocol_version: u16,
    /// AgentSight enforcer build version.
    pub enforcer_version: String,
    /// Exact ActPlane revision used for evaluation.
    pub actplane_revision: String,
}

/// A file operation captured as minimized audit evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAction {
    /// Product policy identifier associated with the observation.
    pub policy_id: String,
    /// Immutable numeric policy revision.
    pub policy_revision: u64,
    /// Normalized operation such as `read` or `open`.
    pub operation: String,
    /// Redacted file path; file contents are never included.
    pub path: String,
    /// Stable resource class such as `credential`.
    pub resource_class: String,
    /// Whether the kernel operation completed successfully.
    pub succeeded: bool,
    /// Observed errno when the operation failed.
    pub errno: Option<i32>,
    /// Backend rule identifier when available.
    pub rule_id: Option<String>,
}

/// Type of process-label state change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaintTransitionKind {
    /// Adds a label after a source match.
    Add,
    /// Propagates a label to a descendant process.
    Inherit,
    /// Expires a label after its configured TTL.
    Expire,
    /// Removes a label during explicit cleanup.
    Clear,
}

/// One process-label lifecycle or propagation fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaintTransition {
    /// Product policy identifier associated with the transition.
    pub policy_id: String,
    /// Immutable numeric policy revision.
    pub policy_revision: u64,
    /// Stable taint label name.
    pub label: String,
    /// Label lifecycle operation.
    pub transition: TaintTransitionKind,
    /// Source process identifier for add or inheritance.
    pub source_pid: i32,
    /// Source start time paired with `source_pid`.
    pub source_process_start_time: u64,
    /// Target process identifier receiving or losing the label.
    pub target_pid: i32,
    /// Target start time paired with `target_pid`.
    pub target_process_start_time: u64,
    /// Sanitized transition explanation.
    pub reason: String,
}

/// Network flow direction observed by the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkDirection {
    /// Connection initiated by the protected process tree.
    Outbound,
    /// Connection accepted by the protected process tree.
    Inbound,
}

/// Stable destination classification used by product policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationClass {
    /// Loopback or Unix-domain destination.
    Local,
    /// Private or link-local address.
    Private,
    /// Explicitly trusted endpoint.
    Trusted,
    /// Public destination not present in the trusted set.
    Public,
    /// Destination could not be classified reliably.
    Unknown,
}

/// Address-family scope requested by a credential-exfiltration policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationScope {
    /// Globally routable IPv4 destinations only.
    PublicIpv4,
    /// Public destinations across all IP families.
    ///
    /// Reserved for capability negotiation; the pinned runtime does not
    /// collect IPv6 connect events and therefore rejects this scope.
    PublicIp,
}

/// Classifies one destination under the supported `public_ipv4` scope.
///
/// Only globally routable IPv4 addresses are public. IPv6, hostnames, and
/// special-purpose IPv4 ranges remain out of scope because the pinned runtime
/// cannot provide complete evidence for them.
pub fn classify_public_ipv4_destination(destination: &str) -> DestinationClass {
    let address = destination.parse::<IpAddr>().ok().or_else(|| {
        destination
            .parse::<SocketAddr>()
            .ok()
            .map(|socket| socket.ip())
    });
    let Some(IpAddr::V4(address)) = address else {
        return DestinationClass::Unknown;
    };
    if address.is_loopback() {
        DestinationClass::Local
    } else if address.is_private() || address.is_link_local() {
        DestinationClass::Private
    } else if is_globally_routable_ipv4(address) {
        DestinationClass::Public
    } else {
        DestinationClass::Unknown
    }
}

fn is_globally_routable_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !address.is_unspecified()
        && !address.is_private()
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_multicast()
        && !address.is_broadcast()
        && !address.is_documentation()
        && first != 0
        && !(first == 100 && (64..=127).contains(&second))
        && !(first == 192 && second == 0 && third == 0)
        && !(first == 192 && second == 88 && third == 99)
        && !(first == 198 && (18..=19).contains(&second))
        && first < 240
}

/// One minimized network operation fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAction {
    /// Product policy identifier associated with the observation.
    pub policy_id: String,
    /// Immutable numeric policy revision.
    pub policy_revision: u64,
    /// Connection direction.
    pub direction: NetworkDirection,
    /// Redacted domain or IP and port.
    pub destination: String,
    /// Policy-facing destination classification.
    pub destination_class: DestinationClass,
    /// Normalized transport such as `tcp` or `udp`.
    pub protocol: String,
    /// Whether the kernel operation completed successfully.
    pub succeeded: bool,
    /// Observed errno when the operation failed.
    pub errno: Option<i32>,
    /// Backend rule identifier when available.
    pub rule_id: Option<String>,
}

/// Progressive rollout mode for one immutable policy revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    /// Counts matches without creating a formal risk case.
    Observe,
    /// Records a risk case while allowing the operation.
    Audit,
    /// Requests kernel denial for a matching operation.
    Enforce,
}

/// One policy match with requested and observed kernel outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    /// Product policy identifier.
    pub policy_id: String,
    /// Immutable numeric policy revision.
    pub policy_revision: u64,
    /// File event that introduced the sensitive label.
    pub source_event_id: Uuid,
    /// Network event that triggered evaluation.
    pub sink_event_id: Uuid,
    /// Progressive rollout mode used for the decision.
    pub mode: PolicyMode,
    /// Effect requested by the compiled policy.
    pub requested_effect: Effect,
    /// Whether the kernel actually rejected the operation.
    pub blocked: bool,
    /// Whether the kernel actually terminated the process.
    pub killed: bool,
    /// Observed errno when the operation failed.
    pub errno: Option<i32>,
    /// Deterministic prioritization score; it does not independently deny.
    pub risk_score: u8,
    /// Sanitized explanation of the matched temporal and exception conditions.
    pub reason: String,
}

/// Enforcement lifecycle or evidence-quality state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnforcementStateEvent {
    /// Optional product policy associated with the state change.
    pub policy_id: Option<String>,
    /// Optional immutable policy revision.
    pub policy_revision: Option<u64>,
    /// Stable machine-readable state code.
    pub code: String,
    /// Whether enforcement remains available after this transition.
    pub ready: bool,
    /// Sanitized operator-facing detail.
    pub message: String,
    /// Number of events known to be lost when reporting evidence degradation.
    pub dropped_events: Option<u64>,
}

/// Product-level credential exfiltration policy independent of ActPlane syntax.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialExfiltrationPolicy {
    /// Stable product policy identifier.
    pub policy_id: String,
    /// Immutable numeric revision.
    pub revision: u64,
    /// Redacted sensitive-source patterns.
    pub source_patterns: Vec<String>,
    /// Destinations exempted from unknown-public handling.
    pub trusted_endpoints: Vec<String>,
    /// Label applied after a sensitive source match.
    pub taint_label: String,
    /// Label lifetime in seconds.
    pub taint_ttl_secs: u64,
    /// Address-family and routability scope evaluated for network sinks.
    pub destination_scope: DestinationScope,
    /// Progressive rollout mode.
    pub mode: PolicyMode,
}

impl CredentialExfiltrationPolicy {
    /// Validates bounded product-policy invariants before adapter translation.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a required identity is empty or a list, TTL,
    /// or revision is outside the supported bounds.
    pub fn validate(&self) -> Result<(), PolicyValidationError> {
        if self.policy_id.trim().is_empty() {
            return Err(PolicyValidationError::EmptyPolicyId);
        }
        if self.revision == 0 {
            return Err(PolicyValidationError::ZeroRevision);
        }
        if self.source_patterns.is_empty() {
            return Err(PolicyValidationError::EmptySources);
        }
        if self.source_patterns.len() > MAX_POLICY_PATTERNS {
            return Err(PolicyValidationError::TooManySourcePatterns);
        }
        if self.trusted_endpoints.len() > MAX_POLICY_PATTERNS {
            return Err(PolicyValidationError::TooManyTrustedEndpoints);
        }
        if self.taint_label.trim().is_empty() {
            return Err(PolicyValidationError::EmptyTaintLabel);
        }
        if self.taint_ttl_secs == 0 {
            return Err(PolicyValidationError::ZeroTaintTtl);
        }
        if self.taint_ttl_secs > 86_400 {
            return Err(PolicyValidationError::TaintTtlTooLarge);
        }
        if self.destination_scope != DestinationScope::PublicIpv4 {
            return Err(PolicyValidationError::UnsupportedDestinationScope);
        }
        Ok(())
    }
}

/// Product-policy validation failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PolicyValidationError {
    /// Policy IDs must contain at least one non-whitespace character.
    #[error("policy ID must not be empty")]
    EmptyPolicyId,
    /// Revision zero is reserved for an unspecified policy.
    #[error("policy revision must be greater than zero")]
    ZeroRevision,
    /// At least one sensitive source is required.
    #[error("at least one source pattern is required")]
    EmptySources,
    /// Source lists are bounded before entering the privileged adapter.
    #[error("source pattern count exceeds {MAX_POLICY_PATTERNS}")]
    TooManySourcePatterns,
    /// Trusted endpoint lists are bounded before adapter translation.
    #[error("trusted endpoint count exceeds {MAX_POLICY_PATTERNS}")]
    TooManyTrustedEndpoints,
    /// Labels are stable policy identities and cannot be blank.
    #[error("taint label must not be empty")]
    EmptyTaintLabel,
    /// TTL zero would make label behavior timing-dependent.
    #[error("taint TTL must be greater than zero")]
    ZeroTaintTtl,
    /// The initial contract limits labels to one day.
    #[error("taint TTL must not exceed 86400 seconds")]
    TaintTtlTooLarge,
    /// IPv6 connect events are absent from the pinned runtime.
    #[error("destination scope is unsupported; expected public_ipv4")]
    UnsupportedDestinationScope,
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
    use super::*;
    use crate::Effect;
    use uuid::Uuid;

    fn fixture_identity() -> EventIdentity {
        EventIdentity {
            binding_id: Uuid::new_v4(),
            agent_id: "agent-1".into(),
            agent_name: Some("Hermes".into()),
            session_id: Some("session-1".into()),
            conversation_id: Some("conversation-1".into()),
            tool_call_id: Some("tool-call-1".into()),
            pid: 4242,
            process_start_time: 101,
            ppid: Some(42),
            cgroup_id: Some(202),
            protocol_version: crate::PROTOCOL_VERSION,
            enforcer_version: "0.1.0".into(),
            actplane_revision: "fixture-revision".into(),
        }
    }

    fn fixture_policy() -> CredentialExfiltrationPolicy {
        CredentialExfiltrationPolicy {
            policy_id: "credential-exfiltration".into(),
            revision: 3,
            source_patterns: vec!["~/.ssh/*".into()],
            trusted_endpoints: vec!["git.example.com:443".into()],
            taint_label: "credential".into(),
            taint_ttl_secs: 300,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Enforce,
        }
    }

    #[test]
    fn policy_decision_round_trip_preserves_requested_and_observed_results() {
        let event = SecurityEvent::policy_decision(
            fixture_identity(),
            PolicyDecision {
                policy_id: "credential-exfiltration".into(),
                policy_revision: 3,
                source_event_id: Uuid::new_v4(),
                sink_event_id: Uuid::new_v4(),
                mode: PolicyMode::Enforce,
                requested_effect: Effect::Block,
                blocked: true,
                killed: false,
                errno: Some(1),
                risk_score: 85,
                reason: "credential taint reached unknown public endpoint".into(),
            },
        );
        let json = serde_json::to_string(&event).expect("fixture should serialize");
        let decoded: SecurityEvent =
            serde_json::from_str(&json).expect("fixture should deserialize");
        assert_eq!(decoded, event);
    }

    #[test]
    fn credential_policy_rejects_zero_ttl() {
        let mut policy = fixture_policy();
        policy.taint_ttl_secs = 0;
        assert_eq!(policy.validate(), Err(PolicyValidationError::ZeroTaintTtl));
    }

    #[test]
    fn credential_policy_rejects_empty_policy_id() {
        let mut policy = fixture_policy();
        policy.policy_id = "  ".into();
        assert_eq!(policy.validate(), Err(PolicyValidationError::EmptyPolicyId));
    }

    #[test]
    fn credential_policy_rejects_zero_revision() {
        let mut policy = fixture_policy();
        policy.revision = 0;
        assert_eq!(policy.validate(), Err(PolicyValidationError::ZeroRevision));
    }

    #[test]
    fn credential_policy_rejects_empty_sources() {
        let mut policy = fixture_policy();
        policy.source_patterns.clear();
        assert_eq!(policy.validate(), Err(PolicyValidationError::EmptySources));
    }

    #[test]
    fn credential_policy_rejects_empty_taint_label() {
        let mut policy = fixture_policy();
        policy.taint_label = "  ".into();
        assert_eq!(
            policy.validate(),
            Err(PolicyValidationError::EmptyTaintLabel)
        );
    }

    #[test]
    fn credential_policy_rejects_ttl_above_one_day() {
        let mut policy = fixture_policy();
        policy.taint_ttl_secs = 86_401;
        assert_eq!(
            policy.validate(),
            Err(PolicyValidationError::TaintTtlTooLarge)
        );
    }

    #[test]
    fn credential_policy_bounds_source_patterns() {
        let mut policy = fixture_policy();
        policy.source_patterns = vec!["redacted".into(); 1_025];
        assert_eq!(
            policy.validate(),
            Err(PolicyValidationError::TooManySourcePatterns)
        );
    }

    #[test]
    fn credential_policy_bounds_trusted_endpoints() {
        let mut policy = fixture_policy();
        policy.trusted_endpoints = vec!["trusted.example:443".into(); 1_025];
        assert_eq!(
            policy.validate(),
            Err(PolicyValidationError::TooManyTrustedEndpoints)
        );
    }

    #[test]
    fn policy_decision_uses_an_explicit_event_type() {
        let event = SecurityEvent::policy_decision(
            fixture_identity(),
            PolicyDecision {
                policy_id: "credential-exfiltration".into(),
                policy_revision: 3,
                source_event_id: Uuid::new_v4(),
                sink_event_id: Uuid::new_v4(),
                mode: PolicyMode::Audit,
                requested_effect: Effect::Notify,
                blocked: false,
                killed: false,
                errno: None,
                risk_score: 60,
                reason: "audit fixture".into(),
            },
        );

        let value = serde_json::to_value(event).expect("fixture should serialize");
        assert_eq!(value["event_type"], "policy_decision");
        assert!(value.get("event").is_some());
    }
}
