use super::*;
use crate::runtime::evidence_state::{RuntimeShellCommandCompleted, ShellEvidenceDelivery};
use crate::runtime::prelude::{
    AgentRunOrigin, CommandStatus, FakeAgentAdapter, OutputRefs, ShellHandoffRequest,
};

// Claiming a recovery is a one-way move, so it must not happen while a
// compaction would drop the resulting internal run: the evidence has to stay
// claimable for a later boundary instead of being silently consumed.
#[test]
fn pending_compaction_does_not_consume_the_shell_evidence_recovery_claim() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    record_pending_recovery_evidence(&mut state, "req-1");
    crate::slash::session::note_compaction_recommendation(
        &mut state,
        "3f2b7c14-8a1d-4c6e-9f05-2b6d8e7a1c43:1:1:100:100",
    );
    assert!(crate::slash::session::compaction_pending_or_active(&state));
    let mut output = Vec::new();

    start_pending_shell_handoff_continuations(&adapter, &mut state, &mut output)
        .expect("guarded scheduling");

    assert!(
        state.agent_run.active.is_none(),
        "no internal run may start while compaction is pending"
    );
    assert_eq!(
        state
            .evidence
            .claim_pending_shell_handoff_continuations()
            .len(),
        1,
        "recovery must still be claimable once compaction clears"
    );
}

#[test]
fn shell_evidence_recovery_starts_at_an_idle_boundary_without_compaction() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    record_pending_recovery_evidence(&mut state, "req-1");
    let mut output = Vec::new();

    start_pending_shell_handoff_continuations(&adapter, &mut state, &mut output)
        .expect("scheduling");

    // The fake adapter can finish the run inside the start path, so the
    // consumed claim — not a still-active run — is what proves the recovery
    // continuation was started.
    assert!(state
        .evidence
        .claim_pending_shell_handoff_continuations()
        .is_empty());
    assert!(!output.is_empty(), "recovery run should render output");
}

// Starting a recovery polls the provider, which can itself surface a
// compaction recommendation that would drop any further internal run. A
// claim cannot be rolled back, so only one recovery may be claimed per
// boundary and the rest must still be claimable afterwards.
#[test]
fn only_one_shell_evidence_recovery_is_claimed_per_idle_boundary() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    record_pending_recovery_evidence(&mut state, "req-1");
    record_pending_recovery_evidence(&mut state, "req-2");
    let mut output = Vec::new();

    start_pending_shell_handoff_continuations(&adapter, &mut state, &mut output)
        .expect("scheduling");

    assert_eq!(
        state
            .evidence
            .claim_pending_shell_handoff_continuations()
            .len(),
        1,
        "the second recovery must survive this boundary unclaimed"
    );
}

// Records one completed shell handoff whose result never reached the
// provider, i.e. exactly the state a recovery continuation is claimed from.
fn record_pending_recovery_evidence(state: &mut InlineState, approval_id: &str) {
    let mut handoff = ShellHandoffRequest::new(
        "df -h",
        "$ df -h",
        "provider-tool-call",
        "agent",
        approval_id,
        "run-1",
        1,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-1".to_string());
    handoff.tool_use_id = Some("toolu-1".to_string());
    let block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "shell-session".to_string(),
        command: "df -h".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 1,
        ended_at_ms: 2,
        duration_ms: 1,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let mut evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::Standard,
    );
    evidence.apply_provider_result_delivery(ShellEvidenceDelivery {
        delivered: false,
        status: "provider_run_not_active",
        recovery_reason: Some("provider run was not active when shell completed"),
        provider_preview_complete: false,
    });
    state.evidence.record_shell_command_completed(evidence);
}
