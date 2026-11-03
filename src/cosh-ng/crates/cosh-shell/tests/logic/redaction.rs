use cosh_shell::adapter::ClaudeCodeAdapter;
use cosh_shell::ledger::build_command_blocks;
use cosh_shell::parser::agent_request_after_confirmation;
use cosh_shell::types::{CoshApprovalMode, ShellEvent, ShellEventKind};

fn provider_prompt(command: &str) -> String {
    let events = [
        ShellEvent::command_started("redaction-logic", "command", command, "/repo", 1),
        ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "redaction-logic",
            "command",
            0,
            2,
            "/unused/output.txt",
        ),
    ];
    let ledger = build_command_blocks(&events);
    assert!(ledger.errors.is_empty(), "redaction ledger valid");
    assert!(ledger.blocks.len() == 1, "redaction command present");
    let mut request =
        agent_request_after_confirmation("redaction-logic", &ledger.blocks[0], &[], true)
            .expect("confirmed redaction request");
    request.context_blocks = ledger.blocks.clone();
    ClaudeCodeAdapter::default()
        .prepare_invocation(&request, CoshApprovalMode::Recommend)
        .prompt
}

#[test]
fn provider_prompt_redacts_extended_secret_assignments_and_flags() {
    let cases = [
        (
            "SECRET_KEY=assignment_canary_4301",
            "assignment_canary_4301",
        ),
        ("secret_key=lowercase_canary_4301", "lowercase_canary_4301"),
        ("private_key='private span canary'", "private span canary"),
        ("access_key=access_canary_4301", "access_canary_4301"),
        ("client_secret=client_canary_4301", "client_canary_4301"),
        (
            "aws_session_token=session_canary_4301",
            "session_canary_4301",
        ),
        ("--auth-token flag_canary_4301", "flag_canary_4301"),
        (
            "--access-key=access_flag_canary_4301",
            "access_flag_canary_4301",
        ),
        ("--private-key 'private flag span'", "private flag span"),
        (
            "--client-secret=client_flag_canary_4301",
            "client_flag_canary_4301",
        ),
    ];
    let leaked = cases
        .iter()
        .filter(|(argument, canary)| {
            provider_prompt(&format!("printf benign-redaction-control {argument}"))
                .contains(*canary)
        })
        .count();
    assert!(leaked == 0, "provider command masking failures: {leaked}");
}

#[test]
fn provider_prompt_preserves_non_secret_command_facts() {
    let command = "printf benign-redaction-control --output report.txt --limit 7";
    let prompt = provider_prompt(command);
    assert!(prompt.contains(command), "ordinary command facts preserved");
    assert!(prompt.contains("exit_code: 0"), "exit fact preserved");
    assert!(
        prompt.contains("terminal-output://redaction-logic/command"),
        "output evidence identity preserved"
    );
}
