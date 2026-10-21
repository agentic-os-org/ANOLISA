//! Recommendation ordering, protection semantics and immutable execution receipts.

use semantic_arbiter::outcome::{
    CandidateControl, Control, Directive, FailurePolicy, Obligation, ObligationAction, Outcome,
    Receipt, ReceiptReason, ReceiptState, SubjectAction, SubjectDirective,
};
use semantic_arbiter::protocol::{
    decode_receipt, decode_request, validate_outcome, CheckRequest, ContractError,
    CHECK_API_VERSION, MAX_MESSAGE_BYTES,
};
use serde_json::json;

fn request() -> CheckRequest {
    decode_request(include_bytes!("../examples/preservation-allow.json")).unwrap()
}

fn audit() -> Obligation {
    Obligation {
        id: "audit".into(),
        critical: true,
        action: ObligationAction::Audit {},
    }
}

fn outcome() -> Outcome {
    Outcome {
        control: Control {
            candidate: CandidateControl::Ask,
            subject: [
                SubjectAction::Freeze,
                SubjectAction::Quarantine,
                SubjectAction::Kill,
            ]
            .into_iter()
            .zip(["freeze", "quarantine", "kill"])
            .map(|(action, id)| SubjectDirective {
                id: id.into(),
                instance_id: "synthetic-instance".into(),
                action,
                scope_ids: vec!["scope-1".into()],
            })
            .collect(),
        },
        obligations: vec![audit()],
        evidence_refs: vec!["rebuild-1".into()],
    }
}

fn receipt() -> Receipt {
    let request = request();
    Receipt {
        api_version: CHECK_API_VERSION.into(),
        request_id: request.request_id,
        binding: request.binding,
        directive_id: "kill".into(),
        state: ReceiptState::Unknown,
        reason: ReceiptReason::Unconfirmed,
    }
}

#[test]
fn candidate_restriction_order_is_monotonic() {
    let order = [
        CandidateControl::Nop,
        CandidateControl::Allow,
        CandidateControl::Ask,
        CandidateControl::Block,
    ];
    assert_eq!(
        serde_json::to_value(order).unwrap(),
        json!(["nop", "allow", "ask", "block"])
    );
    for candidate in order {
        let roundtrip: CandidateControl =
            serde_json::from_slice(&serde_json::to_vec(&candidate).unwrap()).unwrap();
        assert_eq!(roundtrip, candidate);
    }
    for (left_index, left) in order.iter().enumerate() {
        for (right_index, right) in order.iter().enumerate() {
            assert_eq!(left.cmp(right), left_index.cmp(&right_index));
            assert_eq!((*left).max(*right), order[left_index.max(right_index)]);
        }
    }
    let restrictions = [
        CandidateControl::Nop,
        CandidateControl::Block,
        CandidateControl::Allow,
    ];
    assert_eq!(
        restrictions
            .into_iter()
            .fold(CandidateControl::Nop, CandidateControl::max),
        CandidateControl::Block
    );
}

#[test]
fn nop_allow_and_block_are_not_executor_directives() {
    let request = request();
    for candidate in [
        CandidateControl::Nop,
        CandidateControl::Allow,
        CandidateControl::Block,
    ] {
        let plain = Outcome {
            control: Control {
                candidate,
                subject: vec![],
            },
            obligations: if candidate == CandidateControl::Nop {
                vec![]
            } else {
                vec![audit()]
            },
            evidence_refs: vec![],
        };
        assert_eq!(validate_outcome(&plain, &request), Ok(()));
        let directives = plain.directives(&request.binding);
        assert_eq!(directives.len(), plain.obligations.len());
        assert!(directives
            .iter()
            .all(|item| matches!(item, Directive::Obligation(_))));
    }
    let mut ask = outcome();
    ask.control.subject.clear();
    assert_eq!(
        ask.directives(&request.binding),
        vec![
            Directive::Approval {
                id: "candidate-approval".into(),
                candidate_id: "candidate-1".into()
            },
            Directive::Obligation(audit()),
        ]
    );
}

#[test]
fn pending_kill_does_not_absorb_freeze_quarantine_or_obligations() {
    let request = request();
    let outcome = outcome();
    assert_eq!(validate_outcome(&outcome, &request), Ok(()));
    let before = outcome.clone();
    let directives = outcome.directives(&request.binding);
    assert_eq!(
        directives.iter().map(Directive::id).collect::<Vec<_>>(),
        [
            "candidate-approval",
            "freeze",
            "quarantine",
            "kill",
            "audit"
        ]
    );
    for (directive, subject) in directives[1..4].iter().zip(&outcome.control.subject) {
        assert_eq!(directive, &Directive::Subject(subject.clone()));
    }
    assert_eq!(
        outcome, before,
        "Projection must not mutate pending requests"
    );
    let roundtrip: Outcome =
        serde_json::from_slice(&serde_json::to_vec(&outcome).unwrap()).unwrap();
    assert_eq!(roundtrip, outcome);
    for directive in directives {
        let roundtrip: Directive =
            serde_json::from_slice(&serde_json::to_vec(&directive).unwrap()).unwrap();
        assert_eq!(roundtrip, directive);
    }
}

#[test]
fn protection_actions_cannot_be_downgraded_by_the_critical_flag() {
    for action in [
        ObligationAction::Audit {},
        ObligationAction::Snapshot {
            workspace_id: "synthetic-workspace".into(),
        },
        ObligationAction::Rollback {
            workspace_id: "synthetic-workspace".into(),
            checkpoint_id: "checkpoint-1".into(),
        },
        ObligationAction::Cleanup {
            workspace_id: "synthetic-workspace".into(),
            checkpoint_ids: vec!["checkpoint-1".into()],
        },
    ] {
        for critical in [false, true] {
            let obligation = Obligation {
                id: "protection".into(),
                critical,
                action: action.clone(),
            };
            assert_eq!(obligation.failure_policy(), FailurePolicy::Closed);
        }
    }
    for action in [
        ObligationAction::Notify {
            recipient_id: "recipient-1".into(),
        },
        ObligationAction::Diagnose {
            diagnostic_id: "diagnostic-1".into(),
        },
        ObligationAction::RetrieveSkill {
            query_id: "query-1".into(),
        },
        ObligationAction::RetrieveMemory {
            query_id: "query-1".into(),
        },
        ObligationAction::InjectContext {
            material_ids: vec!["material-1".into()],
        },
    ] {
        let mut obligation = Obligation {
            id: "enhancement".into(),
            critical: false,
            action,
        };
        assert_eq!(obligation.failure_policy(), FailurePolicy::Open);
        obligation.critical = true;
        assert_eq!(obligation.failure_policy(), FailurePolicy::Closed);
    }
}

#[test]
fn outcomes_require_one_audit_and_unique_nonreserved_directive_ids() {
    let request = request();
    let original = outcome();
    let mut no_audit = original.clone();
    no_audit.obligations.clear();
    assert_eq!(
        validate_outcome(&no_audit, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut two_audits = original.clone();
    let mut second = audit();
    second.id = "second-audit".into();
    two_audits.obligations.push(second);
    assert_eq!(
        validate_outcome(&two_audits, &request),
        Err(ContractError::InvalidResponse)
    );
    for id in ["candidate-approval", "freeze"] {
        let mut collision = original.clone();
        collision.obligations[0].id = id.into();
        assert_eq!(
            validate_outcome(&collision, &request),
            Err(ContractError::InvalidResponse)
        );
    }
    let mut duplicate = original.clone();
    duplicate.obligations.push(audit());
    assert_eq!(
        validate_outcome(&duplicate, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut duplicate_action = original.clone();
    duplicate_action.control.subject[1].action = SubjectAction::Freeze;
    assert_eq!(
        validate_outcome(&duplicate_action, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut duplicate_id = original.clone();
    duplicate_id.control.subject[1].id = "freeze".into();
    assert_eq!(
        validate_outcome(&duplicate_id, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut reserved = original;
    reserved.control.subject[0].id = "candidate-approval".into();
    assert_eq!(
        validate_outcome(&reserved, &request),
        Err(ContractError::InvalidResponse)
    );
}

#[test]
fn outcomes_reject_wrong_targets_empty_scopes_unknown_refs_and_unbounded_lists() {
    let request = request();
    let original = outcome();
    let mut wrong_instance = original.clone();
    wrong_instance.control.subject[0].instance_id = "another-instance".into();
    assert_eq!(
        validate_outcome(&wrong_instance, &request),
        Err(ContractError::BindingMismatch)
    );
    for scopes in [vec![], vec!["scope-1".into(), "scope-1".into()]] {
        let mut changed = original.clone();
        changed.control.subject[0].scope_ids = scopes;
        assert_eq!(
            validate_outcome(&changed, &request),
            Err(ContractError::InvalidInput)
        );
    }
    let mut too_many_subjects = original.clone();
    too_many_subjects
        .control
        .subject
        .push(original.control.subject[0].clone());
    assert_eq!(
        validate_outcome(&too_many_subjects, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut too_many_obligations = original.clone();
    too_many_obligations.obligations = (0..65)
        .map(|i| Obligation {
            id: format!("notify-{i}"),
            critical: false,
            action: ObligationAction::Notify {
                recipient_id: "recipient-1".into(),
            },
        })
        .collect();
    assert_eq!(
        validate_outcome(&too_many_obligations, &request),
        Err(ContractError::InvalidResponse)
    );
    let mut unknown_ref = original.clone();
    unknown_ref.evidence_refs = vec!["not-current-evidence".into()];
    assert_eq!(
        validate_outcome(&unknown_ref, &request),
        Err(ContractError::InvalidResponse)
    );
    for action in [
        ObligationAction::Snapshot {
            workspace_id: "another-workspace".into(),
        },
        ObligationAction::Rollback {
            workspace_id: "another-workspace".into(),
            checkpoint_id: "checkpoint-1".into(),
        },
        ObligationAction::Cleanup {
            workspace_id: "another-workspace".into(),
            checkpoint_ids: vec!["checkpoint-1".into()],
        },
    ] {
        let mut changed = original.clone();
        changed.obligations.push(Obligation {
            id: "protection".into(),
            critical: true,
            action,
        });
        assert_eq!(
            validate_outcome(&changed, &request),
            Err(ContractError::BindingMismatch)
        );
    }
    for action in [
        ObligationAction::Rollback {
            workspace_id: "synthetic-workspace".into(),
            checkpoint_id: "".into(),
        },
        ObligationAction::Cleanup {
            workspace_id: "synthetic-workspace".into(),
            checkpoint_ids: vec![],
        },
        ObligationAction::Cleanup {
            workspace_id: "synthetic-workspace".into(),
            checkpoint_ids: vec!["checkpoint-1".into(), "checkpoint-1".into()],
        },
        ObligationAction::InjectContext {
            material_ids: vec![],
        },
    ] {
        let mut changed = original.clone();
        changed.obligations.push(Obligation {
            id: "extra".into(),
            critical: false,
            action,
        });
        assert_eq!(
            validate_outcome(&changed, &request),
            Err(ContractError::InvalidInput)
        );
    }
}

#[test]
fn receipt_state_and_reason_pairs_preserve_unknown_without_claiming_application() {
    let request = request();
    let outcome = outcome();
    for state in [
        ReceiptState::Applied,
        ReceiptState::Rejected,
        ReceiptState::Unknown,
    ] {
        for reason in [
            ReceiptReason::Confirmed,
            ReceiptReason::Denied,
            ReceiptReason::Unconfirmed,
        ] {
            let receipt = Receipt {
                state,
                reason,
                ..receipt()
            };
            let decoded =
                decode_receipt(&serde_json::to_vec(&receipt).unwrap(), &request, &outcome);
            let valid = matches!(
                (state, reason),
                (ReceiptState::Applied, ReceiptReason::Confirmed)
                    | (ReceiptState::Rejected, ReceiptReason::Denied)
                    | (ReceiptState::Unknown, ReceiptReason::Unconfirmed)
            );
            if valid {
                assert_eq!(decoded.unwrap(), receipt);
            } else {
                assert_eq!(decoded, Err(ContractError::InvalidResponse));
            }
        }
    }
    let pending = receipt();
    let before = outcome.clone();
    let decoded =
        decode_receipt(&serde_json::to_vec(&pending).unwrap(), &request, &outcome).unwrap();
    assert_eq!(decoded.state, ReceiptState::Unknown);
    assert_eq!(outcome, before);
    assert_eq!(outcome.directives(&request.binding).len(), 5);
}

#[test]
fn receipts_correlate_to_each_original_directive_and_all_binding_dimensions() {
    let request = request();
    let original = outcome();
    for directive in original.directives(&request.binding) {
        let receipt = Receipt {
            directive_id: directive.id().into(),
            ..receipt()
        };
        assert_eq!(
            decode_receipt(&serde_json::to_vec(&receipt).unwrap(), &request, &original).unwrap(),
            receipt
        );
    }
    for path in [
        "/request_id",
        "/directive_id",
        "/binding/session_id",
        "/binding/instance_id",
        "/binding/workspace_id",
        "/binding/task_version",
        "/binding/config_revision",
        "/binding/candidate_id",
        "/binding/candidate_revision",
    ] {
        let mut changed = serde_json::to_value(receipt()).unwrap();
        *changed.pointer_mut(path).unwrap() = json!("different-valid-id");
        assert_eq!(
            decode_receipt(&serde_json::to_vec(&changed).unwrap(), &request, &original),
            Err(ContractError::BindingMismatch),
            "{path}"
        );
    }
    let pending = serde_json::to_vec(&receipt()).unwrap();
    let mut without_kill = original;
    without_kill
        .control
        .subject
        .retain(|subject| subject.action != SubjectAction::Kill);
    assert_eq!(
        decode_receipt(&pending, &request, &without_kill),
        Err(ContractError::BindingMismatch)
    );
    let mut approval = receipt();
    approval.directive_id = "candidate-approval".into();
    without_kill.control.candidate = CandidateControl::Allow;
    assert_eq!(
        decode_receipt(
            &serde_json::to_vec(&approval).unwrap(),
            &request,
            &without_kill
        ),
        Err(ContractError::BindingMismatch)
    );
}

#[test]
fn receipt_decoding_rejects_unknown_duplicate_version_and_oversize_input() {
    let request = request();
    let outcome = outcome();
    for path in ["", "/binding"] {
        let mut changed = serde_json::to_value(receipt()).unwrap();
        changed
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("retry".into(), json!(true));
        assert_eq!(
            decode_receipt(&serde_json::to_vec(&changed).unwrap(), &request, &outcome),
            Err(ContractError::InvalidJson)
        );
    }
    for (field, replacement, error) in [
        (
            "api_version",
            "semantic-arbiter/v1alpha2",
            ContractError::UnsupportedVersion,
        ),
        ("state", "accepted", ContractError::InvalidJson),
        ("reason", "transport_success", ContractError::InvalidJson),
    ] {
        let mut changed = serde_json::to_value(receipt()).unwrap();
        changed[field] = json!(replacement);
        assert_eq!(
            decode_receipt(&serde_json::to_vec(&changed).unwrap(), &request, &outcome),
            Err(error)
        );
    }
    let wire = serde_json::to_string(&receipt()).unwrap();
    let duplicate = wire.replace(
        "\"state\":\"unknown\"",
        "\"state\":\"unknown\",\"state\":\"applied\"",
    );
    assert_eq!(
        decode_receipt(duplicate.as_bytes(), &request, &outcome),
        Err(ContractError::InvalidJson)
    );
    let mut wire = wire.into_bytes();
    wire.resize(MAX_MESSAGE_BYTES, b' ');
    assert!(decode_receipt(&wire, &request, &outcome).is_ok());
    wire.push(b' ');
    assert_eq!(
        decode_receipt(&wire, &request, &outcome),
        Err(ContractError::MessageTooLarge)
    );
}
