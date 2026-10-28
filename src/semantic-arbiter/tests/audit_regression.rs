//! Audit omissions remain invalid even when no candidate restriction is requested.

use semantic_arbiter::outcome::{CandidateControl, Control, Obligation, ObligationAction, Outcome};
use semantic_arbiter::protocol::{decode_request, validate_outcome, ContractError};

fn empty_outcome() -> Outcome {
    Outcome {
        control: Control {
            candidate: CandidateControl::Nop,
            subject: vec![],
        },
        obligations: vec![],
        evidence_refs: vec![],
    }
}

#[test]
fn enabled_coverage_gaps_cannot_disappear_through_nop() {
    let request = decode_request(include_bytes!("../examples/coverage-gap.json")).unwrap();
    let mut outcome = empty_outcome();
    assert_eq!(
        validate_outcome(&outcome, &request),
        Err(ContractError::InvalidResponse)
    );
    outcome.obligations.push(Obligation {
        id: "audit".into(),
        critical: true,
        action: ObligationAction::Audit {},
    });
    validate_outcome(&outcome, &request).unwrap();
}

#[test]
fn referenced_evidence_requires_audit_even_for_nop() {
    let request = decode_request(include_bytes!("../examples/preservation-allow.json")).unwrap();
    let mut outcome = empty_outcome();
    validate_outcome(&outcome, &request).unwrap();
    outcome.evidence_refs.push(request.evidence[0].id.clone());
    assert_eq!(
        validate_outcome(&outcome, &request),
        Err(ContractError::InvalidResponse)
    );
}
