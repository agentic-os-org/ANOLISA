use super::*;

#[path = "runtime_tests/active_agent.rs"]
mod active_agent;
#[path = "runtime_tests/aggregation.rs"]
mod aggregation;
#[path = "runtime_tests/analyze.rs"]
mod analyze;
#[path = "runtime_tests/command_intent.rs"]
mod command_intent;
#[path = "runtime_tests/confidence.rs"]
mod confidence;
#[path = "runtime_tests/consultation_queue.rs"]
mod consultation_queue;
#[path = "runtime_tests/display_policy.rs"]
mod display_policy;
#[path = "runtime_tests/feedback_policy.rs"]
mod feedback_policy;
#[path = "runtime_tests/hint_details.rs"]
mod hint_details;
#[path = "runtime_tests/memory_insight.rs"]
mod memory_insight;
#[path = "runtime_tests/session_input.rs"]
mod session_input;
#[path = "runtime_tests/suppression_budget.rs"]
mod suppression_budget;

fn finding(hook_id: &str, severity: FindingSeverity) -> HookFinding {
    HookFinding {
        hook_id: hook_id.to_string(),
        severity,
        title: format!("{hook_id} title"),
        description: format!("{hook_id} description"),
        suggestion: format!("{hook_id} suggestion"),
        skill: Some("memory-analysis".to_string()),
        cli_hint: Some("free -m".to_string()),
        context_refs: Vec::new(),
    }
}

fn process_finding(title: &str) -> HookFinding {
    let mut finding = finding("high-memory-process", FindingSeverity::Warning);
    finding.title = title.to_string();
    finding
}

fn process_finding_with_severity(title: &str, severity: FindingSeverity) -> HookFinding {
    let mut finding = finding("high-memory-process", severity);
    finding.title = title.to_string();
    finding
}

fn external_finding(hook_id: &str, severity: FindingSeverity, skill: Option<&str>) -> HookFinding {
    let mut finding = finding(hook_id, severity);
    finding.skill = skill.map(String::from);
    finding
}

fn mark_consultation_idle(consultation: &mut PendingConsultation) {
    consultation.queued_at = std::time::Instant::now()
        - SUCCESS_CONSULTATION_IDLE_GRACE
        - std::time::Duration::from_millis(1);
}

fn mark_front_consultation_idle(state: &mut InlineState) {
    let consultation = state
        .hooks
        .pending_consultation_queue
        .front_mut()
        .expect("queued consultation");
    mark_consultation_idle(consultation);
}

fn block(exit_code: i32) -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session".to_string(),
        command: "top -b -n1".to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code,
        status: if exit_code == 0 {
            CommandStatus::Completed
        } else {
            CommandStatus::Failed
        },
        output: OutputRefs {
            terminal_output_ref: Some("/tmp/out".to_string()),
            terminal_output_bytes: 10,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn block_with_command(command: &str) -> CommandBlock {
    let mut block = block(0);
    block.command = command.to_string();
    block
}

fn block_with_command_at(command: &str, ended_at_ms: u64) -> CommandBlock {
    let mut block = block_with_command(command);
    block.ended_at_ms = ended_at_ms;
    block
}

fn write_test_output_ref(name: &str, content: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-hook-runtime-{name}-{}-{}.txt",
        std::process::id(),
        content.len()
    ));
    std::fs::write(&path, content).expect("write hook output");
    path.to_string_lossy().to_string()
}

#[test]
fn command_hook_findings_skip_user_interrupted_exit_one() {
    let mut state = InlineState::default();
    let mut hook_engine = HookEngine::new();
    for hook in default_builtin_hooks() {
        hook_engine.register(hook);
    }
    state.hooks.engine = hook_engine;

    let mut block = block(1);
    block.command = "aliyun configure".to_string();
    block.started_at_ms = 100;
    block.ended_at_ms = 200;

    let started = ShellEvent::command_started("session", "cmd-1", "aliyun configure", "/tmp", 100);
    let mut ctrl_c = ShellEvent::user_input_intercepted("session", "ctrl_c");
    ctrl_c.component = Some("control".to_string());
    ctrl_c.started_at_ms = Some(150);
    let finished = ShellEvent::command_finished(
        ShellEventKind::CommandFailed,
        "session",
        "cmd-1",
        1,
        200,
        "terminal://test/cmd-1",
    );
    let events = vec![started, ctrl_c, finished];

    record_command_hook_findings(&events, &[block], &mut state, 0);

    assert!(state.hooks.findings.is_empty());
}

#[test]
fn command_hook_findings_skip_interactive_cancel_output_without_ctrl_c_event() {
    let mut state = InlineState::default();
    let mut hook_engine = HookEngine::new();
    for hook in default_builtin_hooks() {
        hook_engine.register(hook);
    }
    state.hooks.engine = hook_engine;

    let output_ref =
        write_test_output_ref("sudo-password-required", "sudo: a password is required\n");
    let mut block = block(1);
    block.command = "sudo df -h".to_string();
    block.output.terminal_output_ref = Some(output_ref);

    record_command_hook_findings(&[], &[block], &mut state, 0);

    assert!(state.hooks.findings.is_empty());
}
