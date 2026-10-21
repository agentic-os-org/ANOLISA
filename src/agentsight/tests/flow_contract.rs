//! Public Flow contracts shared by future correlation, capacity, and delivery stages.

#![cfg(target_os = "linux")]

use agentsight::ConnectionId;
use agentsight::capture::{FlowContext, FlowCounters, IncompleteReason, TerminalKind};
use std::collections::{BTreeSet, HashSet};

#[test]
fn context_uses_all_four_identity_fields_and_has_a_stable_key() {
    let connection = ConnectionId {
        pid: 42,
        ssl_ptr: 0xabcd,
    };
    let context = FlowContext::new(connection, 17, 16);
    let contexts = [
        context,
        FlowContext { pid: 43, ..context },
        FlowContext {
            connection: 0xabce,
            ..context
        },
        FlowContext {
            protocol_exchange: 18,
            ..context
        },
        FlowContext {
            connection_generation: 19,
            ..context
        },
    ];
    assert_eq!(context.connection_id(), connection);
    assert_eq!(
        contexts.into_iter().collect::<HashSet<_>>().len(),
        contexts.len()
    );
    assert_eq!(
        contexts.into_iter().collect::<BTreeSet<_>>().len(),
        contexts.len()
    );
    let keys: HashSet<_> = contexts
        .iter()
        .map(|context| context.ledger_key())
        .collect();
    assert_eq!(keys.len(), contexts.len());
    assert_eq!(context.ledger_key(), "42:abcd:17:16");
    assert_eq!(contexts[4].ledger_key(), "42:abcd:17:19");
}

#[test]
fn context_serialization_preserves_exactly_four_fields() {
    let context = FlowContext::new(
        ConnectionId {
            pid: 42,
            ssl_ptr: 0xabcd,
        },
        u64::MAX,
        16,
    );
    let json = serde_json::json!({
        "pid": 42,
        "connection": 43981,
        "protocol_exchange": u64::MAX,
        "connection_generation": 16
    });
    assert_eq!(serde_json::to_value(context).unwrap(), json);
    assert_eq!(
        serde_json::from_value::<FlowContext>(json).unwrap(),
        context
    );
}

#[test]
fn legacy_exchange_alias_defaults_only_the_connection_generation() {
    let legacy = serde_json::json!({
        "pid": 42, "connection": 43981, "exchange": 17
    });
    let context: FlowContext = serde_json::from_value(legacy).unwrap();
    assert_eq!(context, FlowContext::new(context.connection_id(), 17, 0));
    let json = serde_json::to_value(context).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 4);
    assert!(json.get("exchange").is_none());
    assert_eq!(json["protocol_exchange"], 17);
    assert_eq!(json["connection_generation"], 0);
    for invalid in [
        serde_json::json!({"connection": 43981, "exchange": 17}),
        serde_json::json!({"pid": 42, "exchange": 17}),
        serde_json::json!({"pid": 42, "connection": 43981}),
        serde_json::json!({"pid": 42, "connection": 43981, "exchange": -1}),
        serde_json::json!({
            "pid": 42, "connection": 43981,
            "exchange": 17, "protocol_exchange": 18
        }),
    ] {
        assert!(serde_json::from_value::<FlowContext>(invalid).is_err());
    }
}

#[test]
fn terminal_codes_match_their_serialized_contract() {
    for (terminal, code) in [
        (TerminalKind::Complete, "Complete"),
        (TerminalKind::Closed, "Closed"),
        (TerminalKind::TimedOut, "TimedOut"),
        (TerminalKind::Malformed, "Malformed"),
        (TerminalKind::Truncated, "Truncated"),
        (TerminalKind::Dropped, "Dropped"),
    ] {
        assert_eq!(terminal.as_str(), code);
        assert_eq!(serde_json::to_value(terminal).unwrap(), code);
        assert_eq!(
            serde_json::from_value::<TerminalKind>(code.into()).unwrap(),
            terminal
        );
    }
    assert!(serde_json::from_value::<TerminalKind>("Rejected".into()).is_err());
    assert!(serde_json::from_value::<TerminalKind>("complete".into()).is_err());
}

#[test]
fn incomplete_reasons_have_stable_codes_and_deterministic_order() {
    use IncompleteReason::*;
    let cases = [
        (RingBufferLoss, "RingBufferLoss"),
        (RingBufferCoverageUnknown, "RingBufferCoverageUnknown"),
        (FfiOutboundLoss, "FfiOutboundLoss"),
        (RestartUncheckpointed, "RestartUncheckpointed"),
        (QueueCapacity, "QueueCapacity"),
        (SpoolHandoffFull, "SpoolHandoffFull"),
        (WalFull, "WalFull"),
        (WalIo, "WalIo"),
        (WalWriterStopped, "WalWriterStopped"),
        (WalCorruption, "WalCorruption"),
        (FlowByteLimit, "FlowByteLimit"),
        (FlowCacheLimit, "FlowCacheLimit"),
        (FlowIdleTimeout, "FlowIdleTimeout"),
        (FlowLifetimeLimit, "FlowLifetimeLimit"),
        (MalformedProtocol, "MalformedProtocol"),
        (StorageFailure, "StorageFailure"),
        (EmergencyBodyOmitted, "EmergencyBodyOmitted"),
        (EmergencyCapacity, "EmergencyCapacity"),
        (MissingRequest, "MissingRequest"),
        (MissingResponse, "MissingResponse"),
        (MissingEnd, "MissingEnd"),
        (MissingBody, "MissingBody"),
        (UnsupportedProtocol, "UnsupportedProtocol"),
    ];
    let expected: Vec<_> = cases.iter().map(|(reason, _)| *reason).collect();
    let mut reasons = BTreeSet::new();
    for (reason, code) in cases.into_iter().rev() {
        assert_eq!(reason.as_str(), code);
        assert_eq!(serde_json::to_value(reason).unwrap(), code);
        assert_eq!(
            serde_json::from_value::<IncompleteReason>(code.into()).unwrap(),
            reason
        );
        reasons.insert(reason);
        reasons.insert(reason);
    }
    assert_eq!(reasons.into_iter().collect::<Vec<_>>(), expected);
    assert!(serde_json::from_value::<IncompleteReason>("UnknownReason".into()).is_err());
}

#[test]
fn accounting_conserves_admission_pending_terminal_and_unavailable_evidence() {
    let mut counters = FlowCounters::default();
    assert!(counters.is_conserved());
    counters.observed_new = 5;
    counters.admitted = 4;
    counters.rejected = 1;
    counters.active = 4;
    assert!(counters.is_conserved());
    counters.active -= 1;
    counters.pending_coverage += 1;
    assert!(counters.is_conserved());
    // New evidence can revoke a candidate before terminal commit.
    counters.pending_coverage -= 1;
    counters.active += 1;
    assert!(counters.is_conserved());
    counters.active -= 1;
    counters.pending_coverage += 1;
    counters.pending_coverage -= 1;
    counters.complete += 1;
    assert!(counters.is_conserved());
    counters.active -= 1;
    counters.incomplete += 1;
    assert!(counters.is_conserved());
    counters.active -= 1;
    counters.evidence_unavailable += 1;
    assert!(counters.is_conserved());
    counters.active -= 1;
    counters.incomplete += 1;
    assert!(counters.is_conserved());
    assert_eq!(counters.complete + counters.incomplete, 3);
    assert_eq!(
        counters.admitted, 4,
        "unavailable evidence is not a committed terminal"
    );
}

#[test]
fn diagnostic_counts_do_not_become_flow_outcomes() {
    let counters = FlowCounters {
        observed_new: 2,
        admitted: 2,
        complete: 1,
        incomplete: 1,
        late_event_total: 7,
        duplicate_terminal_total: 8,
        ring_loss_suspect_flow_total: 1,
        coverage_unknown_flow_total: 1,
        coverage_sample_missing_total: 11,
        ..FlowCounters::default()
    };
    assert!(counters.is_conserved());
    assert_eq!(
        serde_json::to_value(counters).unwrap(),
        serde_json::json!({
            "observed_new": 2, "admitted": 2, "rejected": 0, "active": 0,
            "pending_coverage": 0, "complete": 1, "incomplete": 1, "evidence_unavailable": 0,
            "late_event_total": 7, "duplicate_terminal_total": 8,
            "ring_loss_suspect_flow_total": 1, "coverage_unknown_flow_total": 1,
            "coverage_sample_missing_total": 11
        })
    );
}

#[test]
fn conservation_rejects_missing_duplicate_and_unadmitted_outcomes() {
    for counters in [
        FlowCounters {
            observed_new: 1,
            ..FlowCounters::default()
        },
        FlowCounters {
            observed_new: 1,
            admitted: 1,
            ..FlowCounters::default()
        },
        FlowCounters {
            observed_new: 1,
            admitted: 1,
            complete: 1,
            active: 1,
            ..FlowCounters::default()
        },
        FlowCounters {
            observed_new: 1,
            rejected: 1,
            incomplete: 1,
            ..FlowCounters::default()
        },
    ] {
        assert!(!counters.is_conserved(), "{counters:?}");
    }
}

#[test]
fn conservation_rejects_overflow_in_both_equations() {
    let admission_overflow = FlowCounters {
        admitted: u64::MAX,
        rejected: 1,
        active: u64::MAX,
        ..FlowCounters::default()
    };
    assert!(!admission_overflow.is_conserved());
    for bucket in 0..5 {
        let mut values = [0; 5];
        values[bucket] = u64::MAX;
        values[(bucket + 1) % 5] = 1;
        let [
            active,
            pending_coverage,
            complete,
            incomplete,
            evidence_unavailable,
        ] = values;
        let counters = FlowCounters {
            active,
            pending_coverage,
            complete,
            incomplete,
            evidence_unavailable,
            ..FlowCounters::default()
        };
        assert!(!counters.is_conserved(), "{counters:?}");
    }
    assert!(
        FlowCounters {
            observed_new: u64::MAX,
            admitted: u64::MAX,
            complete: u64::MAX,
            ..FlowCounters::default()
        }
        .is_conserved()
    );
}
