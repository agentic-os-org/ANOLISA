use super::*;

#[test]
fn raw_cli_details_approvals_renders_decision_journal_panel() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? request tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(2_500)),
        (
            b"/details approvals\n".to_vec(),
            Duration::from_millis(1_500),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(500)),
    ]);

    assert!(output.contains("Approval journal"), "{output}");
    assert!(output.contains("1 decisions"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("approved"),
        "{output}"
    );
    assert!(
        !(output.contains("req-2") && output.contains("denied")),
        "{output}"
    );
    assert!(output.contains("Command: $ git status"), "{output}");
    assert!(
        output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("Command block:"), "{output}");
    assert!(output.contains("Redaction: ref_only"), "{output}");
    assert!(
        !output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_approvals_records_denied_not_executed() {
    let output = run_raw_cli_ask_with_args_and_marker_input(
        &[],
        &[
            ("cosh-osc$", b"?? stream tool approval\n"),
            ("Approval req-1", b"\x1b[C\x1b[C\n"),
            (
                "Command was not executed for req-1",
                b"/details approvals\n",
            ),
            ("Approval journal", b"exit\n"),
        ],
    );

    assert!(output.contains("Denied req-1"), "{output}");
    assert!(output.contains("Approval journal"), "{output}");
    assert!(output.contains("1 decisions"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("denied"),
        "{output}"
    );
    assert!(
        output.contains("Execution: not_executed_denied"),
        "{output}"
    );
    assert!(
        !output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("Command block: <none>"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_approvals_records_cancelled_not_executed() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"\x1b\n".to_vec(), Duration::from_millis(2_500)),
        (
            b"/details approvals\n".to_vec(),
            Duration::from_millis(1_000),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(500)),
    ]);

    assert!(output.contains("Cancelled req-1"), "{output}");
    assert!(output.contains("Approval journal"), "{output}");
    assert!(output.contains("1 decisions"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("cancelled"),
        "{output}"
    );
    assert!(
        output.contains("Execution: not_executed_cancelled"),
        "{output}"
    );
    assert!(
        !output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("Command block: <none>"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_approvals_uses_zh_language_env() {
    let output = run_raw_cli_ask_with_args_env_and_delayed_input(
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? request tool approval\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(2_500)),
            (
                b"/details approvals\n".to_vec(),
                Duration::from_millis(1_500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("审批记录"), "{output}");
    assert!(output.contains("1 条决策"), "{output}");
    assert!(
        output.contains("req-1") && output.contains("approved"),
        "{output}"
    );
    assert!(output.contains("命令: $ git status"), "{output}");
    assert!(output.contains("执行: foreground_shell_pty"), "{output}");
    assert!(output.contains("命令块:"), "{output}");
    assert!(output.contains("脱敏: ref_only"), "{output}");
    assert!(!output.contains("Approval journal"), "{output}");
    assert!(!output.contains("Command: $ git status"), "{output}");
    assert!(
        !output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(!output.contains("Command block:"), "{output}");
    assert!(!output.contains("Redaction: ref_only"), "{output}");
    assert!(
        !output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, APPROVAL_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_details_for_approval_uses_structured_panel() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"d".to_vec(), Duration::from_millis(2_500)),
        (b"\x1b".to_vec(), Duration::from_millis(300)),
        (b"exit\n".to_vec(), Duration::from_millis(800)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("tool request"), "{output}");
    assert!(output.contains("Cancelled req-1"), "{output}");
    assert!(output.contains("medium risk"), "{output}");
    assert!(
        output.contains("Policy: user approval is required before any executable tool request"),
        "{output}"
    );
    assert!(
        output.contains("Keys:") && output.contains("d details"),
        "{output}"
    );
    assert!(output.contains("Command:"), "{output}");
    assert!(output.contains("git status"), "{output}");
    assert!(!output.contains("Subject: tool shell"), "{output}");
    assert!(!output.contains("id: req-1"), "{output}");
    assert!(!output.contains("preview: git status"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_details_for_approval_uses_zh_language_env() {
    let output = run_raw_cli_ask_with_args_env_and_delayed_input(
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
            (b"d".to_vec(), Duration::from_millis(2_500)),
            (b"\x1b".to_vec(), Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(800)),
        ],
    );

    assert_zh_approval_prompt_visible(&output);
    assert!(output.contains("已取消 req-1"), "{output}");
    assert!(output.contains("Bash · 中风险"), "{output}");
    assert!(!output.contains("风险 medium"), "{output}");
    assert!(
        output.contains("策略: 可执行 tool 请求必须先经过用户审批。"),
        "{output}"
    );
    assert!(output.contains("命令:"), "{output}");
    assert!(output.contains("git status"), "{output}");
    assert!(!output.contains("Approval required"), "{output}");
    assert!(!output.contains("Approval req-1"), "{output}");
    assert!(!output.contains("Approval details"), "{output}");
    assert!(!output.contains("Cancelled req-1"), "{output}");
    assert!(
        !output.contains("Policy: user approval is required"),
        "{output}"
    );
    assert!(!output.contains("Keys:"), "{output}");
    assert!(!output.contains("Command:"), "{output}");
    assert!(!output.contains("Subject: tool shell"), "{output}");
    assert!(!output.contains("id: req-1"), "{output}");
    assert!(!output.contains("preview: git status"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, APPROVAL_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_multiline_bash_tool_is_visible_in_approval_details() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (
            b"?? stream multiline tool approval\n".to_vec(),
            Duration::ZERO,
        ),
        (b"d".to_vec(), Duration::from_millis(2_500)),
        (b"\x1b".to_vec(), Duration::from_millis(300)),
        (b"exit\n".to_vec(), Duration::from_millis(200)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("Command:"), "{output}");
    assert!(output.contains("printf one"), "{output}");
    assert!(output.contains("printf two"), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}
