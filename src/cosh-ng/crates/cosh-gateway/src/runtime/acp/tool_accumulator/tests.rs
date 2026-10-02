use cosh_gateway_contracts::runtime::ExecutionAuthority;
use serde_json::json;

use super::*;

fn accumulator(max_invocations: usize) -> ToolInvocationAccumulator {
    ToolInvocationAccumulator::new(
        AcpToolAccumulatorLimits {
            max_invocations,
            max_identifier_bytes: 64,
            max_payload_bytes: 4096,
        },
        ExecutionAuthority::ProviderNativeObserved,
    )
    .expect("test limits are valid")
}

#[test]
fn create_and_updates_keep_stable_identity_and_revision() {
    let mut accumulator = accumulator(4);
    let turn_id = TurnId::new();
    let created = accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Inspect repository",
                "kind": "read"
            }),
        )
        .expect("create is valid");
    let AcpToolAccumulation::Updated(created) = created else {
        panic!("expected created snapshot");
    };
    assert_eq!(created.projection.revision, 1);
    assert_eq!(created.projection.status, ToolInvocationStatus::Pending);
    assert_eq!(
        created.projection.authority,
        ExecutionAuthority::ProviderNativeObserved
    );

    let updated = accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "in_progress",
                "rawInput": {"path": "."}
            }),
        )
        .expect("update is valid");
    let AcpToolAccumulation::Updated(updated) = updated else {
        panic!("expected updated snapshot");
    };
    assert_eq!(
        updated.projection.tool_use_id,
        created.projection.tool_use_id
    );
    assert_eq!(updated.projection.revision, 2);
    assert_eq!(updated.projection.status, ToolInvocationStatus::InProgress);
    assert_eq!(updated.tool_call["rawInput"]["path"], ".");
}

#[test]
fn presentation_summary_strips_controls_and_stays_bounded() {
    let mut accumulator = ToolInvocationAccumulator::new(
        AcpToolAccumulatorLimits {
            max_invocations: 1,
            max_identifier_bytes: 64,
            max_payload_bytes: MAX_TEXT_BYTES * 2,
        },
        ExecutionAuthority::ProviderNativeObserved,
    )
    .unwrap();
    let title = format!("Inspect\u{1b}[31m{}", "x".repeat(MAX_TEXT_BYTES));
    let observed = accumulator
        .observe(
            "session-1",
            &TurnId::new(),
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": title,
                "kind": "read"
            }),
        )
        .unwrap();
    let AcpToolAccumulation::Updated(snapshot) = observed else {
        panic!("expected tool snapshot");
    };
    assert!(!snapshot
        .projection
        .summary
        .summary
        .as_str()
        .contains('\u{1b}'));
    assert!(snapshot.projection.summary.summary.as_str().len() <= MAX_TEXT_BYTES);
}

#[test]
fn update_before_create_is_buffered_and_merged() {
    let mut accumulator = accumulator(4);
    let turn_id = TurnId::new();
    let buffered = accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "in_progress",
                "rawInput": {"path": "README.md"}
            }),
        )
        .expect("partial update is valid");
    assert!(matches!(buffered, AcpToolAccumulation::Buffered { .. }));

    let created = accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Read a file",
                "kind": "read"
            }),
        )
        .expect("later create is valid");
    let AcpToolAccumulation::Updated(snapshot) = created else {
        panic!("expected merged snapshot");
    };
    assert_eq!(snapshot.projection.status, ToolInvocationStatus::InProgress);
    assert_eq!(snapshot.tool_call["rawInput"]["path"], "README.md");
}

#[test]
fn permission_carrier_promotes_only_a_truly_absent_invocation() {
    let carrier = json!({
        "toolCallId": "call-1",
        "title": "Read a file",
        "kind": "read",
        "status": "pending",
        "rawInput": {"path": "README.md"}
    });
    let turn_id = TurnId::new();
    let mut promoted = accumulator(1);
    let snapshot = promoted
        .promote_permission_carrier("session-1", &turn_id, &carrier)
        .expect("complete carrier is promotable");
    assert_eq!(snapshot.projection.revision, 1);
    assert_eq!(snapshot.projection.status, ToolInvocationStatus::Pending);
    assert_eq!(snapshot.tool_call["rawInput"]["path"], "README.md");

    let mut buffered = accumulator(1);
    assert!(matches!(
        buffered.observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "rawInput": {"path": "substituted.md"}
            })
        ),
        Ok(AcpToolAccumulation::Buffered { .. })
    ));
    assert!(matches!(
        buffered.promote_permission_carrier("session-1", &turn_id, &carrier),
        Err(AcpToolAccumulatorError::ConflictingCreate { .. })
    ));
}

#[test]
fn permission_refinement_preserves_identity_and_advances_revision() {
    let mut accumulator = accumulator(1);
    let turn_id = TurnId::new();
    let AcpToolAccumulation::Updated(observed) = accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "command-1",
                "title": "Read file '/workspace/proof.txt'",
                "kind": "read",
                "status": "in_progress",
                "locations": [{"path": "/workspace/proof.txt"}]
            }),
        )
        .unwrap()
    else {
        panic!("expected initial command-action projection");
    };
    let refined = accumulator
        .refine_in_progress_permission_carrier(
            "session-1",
            &turn_id,
            &json!({
                "toolCallId": "command-1",
                "title": "Run cwd=\"/workspace\", command=\"cat proof.txt\"",
                "kind": "execute",
                "status": "pending",
                "rawInput": {"command": "cat proof.txt", "cwd": "/workspace"}
            }),
        )
        .unwrap();
    assert_eq!(
        refined.projection.tool_use_id,
        observed.projection.tool_use_id
    );
    assert_eq!(
        refined.projection.revision,
        observed.projection.revision + 1
    );
    assert_eq!(refined.projection.status, ToolInvocationStatus::Pending);
    assert_eq!(refined.projection.summary.name.as_str(), "execute");
    assert_eq!(refined.tool_call["rawInput"]["command"], "cat proof.txt");
}

#[test]
fn permission_refinement_rejects_absent_buffered_and_terminal_calls() {
    let carrier = json!({
        "toolCallId": "command-1",
        "title": "Run command=\"pwd\"",
        "kind": "execute",
        "status": "pending",
        "rawInput": {"command": "pwd"}
    });
    let turn_id = TurnId::new();
    let mut absent = accumulator(1);
    assert!(matches!(
        absent.refine_in_progress_permission_carrier("session-1", &turn_id, &carrier),
        Err(AcpToolAccumulatorError::ConflictingCreate { .. })
    ));

    let mut buffered = accumulator(1);
    buffered
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "command-1",
                "rawInput": {"command": "substituted"}
            }),
        )
        .unwrap();
    assert!(matches!(
        buffered.refine_in_progress_permission_carrier("session-1", &turn_id, &carrier),
        Err(AcpToolAccumulatorError::ConflictingCreate { .. })
    ));

    let mut terminal = accumulator(1);
    terminal
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "command-1",
                "title": "Read",
                "kind": "read",
                "status": "completed"
            }),
        )
        .unwrap();
    assert!(matches!(
        terminal.refine_in_progress_permission_carrier("session-1", &turn_id, &carrier),
        Err(AcpToolAccumulatorError::ConflictingCreate { .. })
    ));
}

#[test]
fn duplicate_is_idempotent_but_conflicting_create_fails() {
    let mut accumulator = accumulator(4);
    let turn_id = TurnId::new();
    let created = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call-1",
        "title": "Run tests",
        "kind": "execute"
    });
    accumulator
        .observe("session-1", &turn_id, &created)
        .expect("first create is valid");
    assert!(matches!(
        accumulator.observe("session-1", &turn_id, &created),
        Ok(AcpToolAccumulation::Unchanged { .. })
    ));
    let conflicting = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call-1",
        "title": "Delete files",
        "kind": "delete"
    });
    assert!(matches!(
        accumulator.observe("session-1", &turn_id, &conflicting),
        Err(AcpToolAccumulatorError::ConflictingCreate { .. })
    ));
}

#[test]
fn terminal_calls_allow_replay_but_reject_mutation() {
    let mut accumulator = accumulator(4);
    let turn_id = TurnId::new();
    accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Run tests",
                "status": "completed"
            }),
        )
        .expect("terminal create is valid");
    let replay = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-1",
        "status": "completed"
    });
    assert!(matches!(
        accumulator.observe("session-1", &turn_id, &replay),
        Ok(AcpToolAccumulation::Unchanged { .. })
    ));
    let mutation = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-1",
        "title": "changed after completion"
    });
    assert!(matches!(
        accumulator.observe("session-1", &turn_id, &mutation),
        Err(AcpToolAccumulatorError::TerminalMutation { .. })
    ));
}

#[test]
fn status_cannot_regress_from_in_progress_to_pending() {
    let mut accumulator = accumulator(4);
    let turn_id = TurnId::new();
    accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Run tests",
                "status": "in_progress"
            }),
        )
        .expect("in-progress create is valid");

    assert!(matches!(
        accumulator.observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "pending"
            })
        ),
        Err(AcpToolAccumulatorError::StatusRegression {
            from: "in_progress",
            to: "pending"
        })
    ));
}

#[test]
fn revision_overflow_fails_explicitly() {
    let mut accumulator = accumulator(4);
    let turn_id = TurnId::new();
    accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Run tests"
            }),
        )
        .expect("create is valid");
    accumulator
        .invocations
        .values_mut()
        .next()
        .expect("test invocation exists")
        .revision = u64::MAX;

    assert!(matches!(
        accumulator.observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "in_progress"
            })
        ),
        Err(AcpToolAccumulatorError::RevisionOverflow)
    ));
}

#[test]
fn identity_is_scoped_by_session_and_turn() {
    let mut accumulator = accumulator(4);
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let update = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "reused",
        "title": "Read"
    });
    let AcpToolAccumulation::Updated(first) = accumulator
        .observe("session-1", &first_turn, &update)
        .expect("first turn is valid")
    else {
        panic!("expected first snapshot");
    };
    let AcpToolAccumulation::Updated(second) = accumulator
        .observe("session-1", &second_turn, &update)
        .expect("second turn is valid")
    else {
        panic!("expected second snapshot");
    };
    assert_ne!(first.projection.tool_use_id, second.projection.tool_use_id);
}

#[test]
fn state_and_payload_are_bounded() {
    let mut accumulator = accumulator(1);
    let turn_id = TurnId::new();
    accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Read"
            }),
        )
        .expect("first invocation fits");
    assert!(matches!(
        accumulator.observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-2",
                "title": "Read"
            })
        ),
        Err(AcpToolAccumulatorError::TooManyInvocations { limit: 1 })
    ));

    let mut small_payload = ToolInvocationAccumulator::new(
        AcpToolAccumulatorLimits {
            max_invocations: 1,
            max_identifier_bytes: 64,
            max_payload_bytes: 64,
        },
        ExecutionAuthority::CoshBrokered,
    )
    .expect("test limits are valid");
    assert!(matches!(
        small_payload.observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "x".repeat(128)
            })
        ),
        Err(AcpToolAccumulatorError::PayloadTooLarge { limit: 64 })
    ));
}

#[test]
fn accumulated_partial_updates_share_one_payload_bound() {
    let mut accumulator = ToolInvocationAccumulator::new(
        AcpToolAccumulatorLimits {
            max_invocations: 1,
            max_identifier_bytes: 64,
            max_payload_bytes: 220,
        },
        ExecutionAuthority::ProviderNativeObserved,
    )
    .expect("test limits are valid");
    let turn_id = TurnId::new();
    accumulator
        .observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "rawInput": "x".repeat(100)
            }),
        )
        .expect("first partial update fits");

    assert!(matches!(
        accumulator.observe(
            "session-1",
            &turn_id,
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "rawOutput": "y".repeat(100)
            })
        ),
        Err(AcpToolAccumulatorError::PayloadTooLarge { limit: 220 })
    ));
}

#[test]
fn release_turn_recovers_capacity() {
    let mut accumulator = accumulator(1);
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let update = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call-1",
        "title": "Read"
    });
    accumulator
        .observe("session-1", &first_turn, &update)
        .expect("first invocation fits");
    accumulator.release_turn("session-1", &first_turn);
    assert!(matches!(
        accumulator.observe("session-1", &second_turn, &update),
        Ok(AcpToolAccumulation::Updated(_))
    ));
}
