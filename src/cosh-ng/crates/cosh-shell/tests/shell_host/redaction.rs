use super::*;
use cosh_shell::adapter::ClaudeCodeAdapter;
use cosh_shell::types::CoshApprovalMode;

#[test]
fn scripted_bash_redacts_command_facts_and_durable_output_evidence() {
    assert!(
        Command::new("bash").arg("--version").output().is_ok(),
        "bash required for redaction PTY evidence"
    );
    let work_dir = tempfile::tempdir().expect("redaction PTY work dir");
    let config = ShellHostConfig::new("redaction-pty", work_dir.path());
    let cases = [
        (
            "printf '%s\\n' 'SECRET_KEY=pty_assignment_canary' 'benign-redaction-control'",
            "pty_assignment_canary",
        ),
        (
            "printf '%s\\n' 'private_key=\"private pty canary span\"' 'benign-redaction-control'",
            "private pty canary span",
        ),
        (
            "printf '%s\\n' --auth-token pty_flag_canary 'benign-redaction-control'",
            "pty_flag_canary",
        ),
        (
            "printf '%s\\n' 'access_key=pty_access_canary' 'benign-redaction-control'",
            "pty_access_canary",
        ),
    ];
    let mut inputs = cases
        .iter()
        .map(|(command, _)| ScriptedInput::command(*command))
        .collect::<Vec<_>>();
    inputs.push(ScriptedInput::command(
        "printf '%s\\n' 'benign-redaction-control'",
    ));
    let output = run_scripted_bash(&config, &inputs).expect("redaction scripted PTY");
    let events = read_shell_events(&output.journal_path).expect("redaction journal replay");
    let ledger = build_command_blocks(&events);
    assert!(ledger.errors.is_empty(), "redaction PTY ledger valid");
    let blocks = ledger
        .blocks
        .iter()
        .filter(|block| (1..=inputs.len()).any(|index| block.id == format!("cmd-{index}")))
        .collect::<Vec<_>>();
    assert!(
        blocks.len() == inputs.len(),
        "all redaction PTY commands captured"
    );
    assert!(
        blocks
            .iter()
            .enumerate()
            .all(|(index, block)| block.id == format!("cmd-{}", index + 1)),
        "scripted command order retained"
    );
    assert!(
        blocks.iter().all(|block| block.exit_code == 0),
        "PTY commands executed"
    );

    let mut request = agent_request_after_confirmation("redaction-pty", blocks[0], &[], true)
        .expect("redaction PTY confirmed request");
    request.context_blocks = blocks.iter().map(|block| (**block).clone()).collect();
    let prompt = ClaudeCodeAdapter::default()
        .prepare_invocation(&request, CoshApprovalMode::Recommend)
        .prompt;
    let prompt_leaks = cases
        .iter()
        .filter(|(_, canary)| prompt.contains(*canary))
        .count();

    let mut durable_leaks = 0;
    let mut preserved_outputs = 0;
    for block in &blocks {
        let path = block
            .output
            .terminal_output_ref
            .as_deref()
            .expect("redaction output evidence ref");
        let persisted = std::fs::read_to_string(path).expect("redaction evidence readback");
        durable_leaks += cases
            .iter()
            .filter(|(_, canary)| persisted.contains(*canary))
            .count();
        preserved_outputs += usize::from(persisted.contains("benign-redaction-control"));
    }
    assert!(
        preserved_outputs == inputs.len(),
        "ordinary PTY output retained on every durable evidence ref"
    );
    assert!(
        prompt.contains("benign-redaction-control"),
        "ordinary provider context retained"
    );
    assert!(
        prompt_leaks == 0 && durable_leaks == 0,
        "PTY masking failures: provider={prompt_leaks}, durable={durable_leaks}"
    );
}
