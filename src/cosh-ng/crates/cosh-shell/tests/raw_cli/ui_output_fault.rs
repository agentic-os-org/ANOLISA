use crate::support::ui_approval_slice::{RealCoreApprovalHarness, ServerScript, SliceLaunch};
use crate::support::ui_output_fault::OutputFault;

#[test]
fn committed_handoff_survives_persistent_output_failure_and_recovers_without_input_leak() {
    let fault = OutputFault::build();
    let launch = SliceLaunch {
        server_script: ServerScript::DelayedEffect,
        output_fault_library: Some(fault.path().to_owned()),
        rows: 32,
        ..SliceLaunch::activated()
    };
    let mut harness = RealCoreApprovalHarness::spawn(launch);
    assert!(
        harness.request_approval_card(),
        "real worker approval must be visible before the fault"
    );
    assert_eq!(
        harness
            .worker_descendants_within(std::time::Duration::from_millis(300))
            .len(),
        1,
        "actual live worker before receipt failure"
    );
    harness
        .persistent_output_fault_and_recovery(false)
        .check_inline_baseline()
        .expect("persistent output failure must preserve real effect, host result and Bash");
}

#[test]
fn committed_handoff_output_failure_preserves_existing_timeout_settlement() {
    let fault = OutputFault::build();
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch {
        server_script: ServerScript::TimeoutEffect,
        output_fault_library: Some(fault.path().to_owned()),
        handoff_timeout_secs: Some(2),
        rows: 32,
        ..SliceLaunch::activated()
    });
    assert!(harness.request_approval_card());
    assert_eq!(
        harness
            .worker_descendants_within(std::time::Duration::from_millis(300))
            .len(),
        1
    );
    let observation = harness.persistent_output_fault_and_recovery(true);
    assert_eq!(observation.effect_count, 0);
    assert_eq!(observation.audit_event_count("approval.resolved"), 1);
}

#[test]
fn committed_handoff_survives_gate_withdraw_write_failure_after_audit_resolution() {
    let fault = OutputFault::build();
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch {
        server_script: ServerScript::DelayedEffect,
        output_fault_library: Some(fault.path().to_owned()),
        output_fault_before_receipt: true,
        rows: 32,
        ..SliceLaunch::activated()
    });
    assert!(harness.request_approval_card());
    assert_eq!(
        harness
            .worker_descendants_within(std::time::Duration::from_millis(300))
            .len(),
        1
    );
    harness
        .persistent_output_fault_and_recovery(false)
        .check_inline_baseline()
        .expect("physical Gate withdrawal failure must not abandon the approved effect");
}

#[test]
fn native_prompt_recovers_without_input_after_a_second_physical_write_failure() {
    let fault = OutputFault::build_with_prompt_refault();
    let mut harness = RealCoreApprovalHarness::spawn(SliceLaunch {
        server_script: ServerScript::DelayedEffect,
        output_fault_library: Some(fault.path().to_owned()),
        rows: 32,
        ..SliceLaunch::activated()
    });
    assert!(harness.request_approval_card());
    assert_eq!(
        harness
            .worker_descendants_within(std::time::Duration::from_secs(5))
            .len(),
        1
    );
    let observation = harness.persistent_output_fault_and_recovery(false);
    observation
        .check_inline_baseline()
        .expect("a second physical failure must retain Bash and restore its original prompt");
    harness.assert_native_prompt_refault_was_injected();
}
