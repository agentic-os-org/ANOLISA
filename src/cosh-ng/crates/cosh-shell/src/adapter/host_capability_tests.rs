use super::*;
use crate::types::{AgentMode, CommandBlock, CommandStatus, OutputRefs};

fn capability_test_request() -> AgentRequest {
    AgentRequest {
        id: "cap-run".to_string(),
        session_id: "cap-session".to_string(),
        command_block: CommandBlock {
            id: "cap-blk".to_string(),
            session_id: "cap-session".to_string(),
            command: "echo cap".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 0,
            ended_at_ms: 0,
            duration_ms: 0,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: vec![],
        context_hints: vec![],
        user_input: Some("cap".to_string()),
        findings: vec![],
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

#[test]
fn an_acp_run_declares_the_host_executed_terminal_fact() {
    // The host-shell extension is the wire for the same terminal fact the
    // control protocol calls a host-executed shell result. Without the
    // declaration the runtime stops the run at approval time and the
    // foreground authority can only report an unknown terminal state.
    let request = capability_test_request();
    let acp = start_threaded_adapter_run(
        AdapterInstance::Acp(AcpAgentAdapter::default()),
        request.clone(),
    );
    assert!(
        acp.control_capabilities()
            .can_handle_host_executed_shell_tool_result,
        "an ACP run owns the host-executed terminal fact"
    );

    let legacy = start_threaded_adapter_run(
        AdapterInstance::CoshCore(CoshCoreAdapter::default()),
        request,
    );
    assert!(
        !legacy
            .control_capabilities()
            .can_handle_host_executed_shell_tool_result,
        "a backend that never advertised the capability must not claim it"
    );
}
