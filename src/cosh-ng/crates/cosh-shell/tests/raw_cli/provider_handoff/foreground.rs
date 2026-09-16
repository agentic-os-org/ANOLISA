use super::*;

#[test]
fn raw_cli_control_shell_permission_uses_foreground_and_suppresses_provider_output() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? provider native tool\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(2_500)),
            (
                b"/details tool-1\n/details out-1\n/details approvals\nexit\n".to_vec(),
                Duration::from_millis(4_500),
            ),
        ],
    );

    assert_approval_prompt_visible(&output);
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("provider-shell-handoff"), "{output}");
    assert!(output.contains("Activity details tool-1"), "{output}");
    assert!(
        output.contains("Tool - Bash requested: $ printf 'provider-shell-handoff"),
        "{output}"
    );
    assert!(output.contains("Details unavailable"), "{output}");
    assert!(output.contains("out-1 is not available"), "{output}");
    assert!(
        output.contains("Execution: foreground_shell_pty"),
        "{output}"
    );
    assert!(!output.contains("Activity details out-1"), "{output}");
    assert!(
        !output.contains("PROVIDER NATIVE OUTPUT RENDERED AFTER ALLOW"),
        "{output}"
    );
    assert!(
        !output.contains("Tool output - stdout captured"),
        "{output}"
    );
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_auto_provider_shell_permission_uses_foreground_handoff() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider auto safe shell\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );

    assert!(output.contains("Mode set to auto."), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("Approval required"), "{output}");
    assert!(output.contains("Filesystem"), "{output}");
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(
        !output.contains("PROVIDER AUTO NATIVE OUTPUT RENDERED AFTER ALLOW"),
        "{output}"
    );
}

#[test]
fn raw_cli_control_shell_output_uses_foreground_transcript_by_default() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider auto safe shell\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("Filesystem"), "{output}");
    assert!(
        !output.contains("PROVIDER AUTO NATIVE OUTPUT RENDERED AFTER ALLOW"),
        "{output}"
    );
    assert!(
        !output.contains("Tool output: stdout captured; [Details] out-1"),
        "{output}"
    );
    assert!(!output.contains("Tool success"), "{output}");
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
}

#[test]
fn raw_cli_zh_control_shell_foreground_localizes_shell_owned_wrapper() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider auto safe shell\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );

    assert!(output.contains("模式已设置为 auto。"), "{output}");
    assert!(
        output.contains("只读工具会自动批准；高风险请求仍需确认。"),
        "{output}"
    );
    assert!(output.contains("已自动批准 req-1"), "{output}");
    assert!(output.contains("Bash tool 已发送到 shell"), "{output}");
    assert!(
        !output.contains("PROVIDER AUTO NATIVE OUTPUT RENDERED AFTER ALLOW"),
        "{output}"
    );
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(!output.contains("Activity details out-1"), "{output}");
    assert!(
        !output.contains("Read-only tools auto-approved; risky requests need confirmation."),
        "{output}"
    );
    assert!(
        !output.contains("已允许 provider-native shell tool 执行"),
        "{output}"
    );
    assert_no_migrated_english_ui_labels(&output, PROVIDER_NATIVE_ZH_FORBIDDEN_UI);
    assert_no_migrated_english_ui_labels(&output, DETAILS_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_zh_control_shell_details_localizes_shell_owned_chrome() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? provider native tool\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(2_500)),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(3_000)),
            (b"/details out-1\n".to_vec(), Duration::from_millis(500)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("已批准 req-1"), "{output}");
    assert!(output.contains("Bash tool 已发送到 shell"), "{output}");
    assert!(output.contains("活动详情 tool-1"), "{output}");
    assert!(
        output.contains("Bash 请求审批：$ printf 'provider-shell-handoff"),
        "{output}"
    );
    assert!(output.contains("详情不可用"), "{output}");
    assert!(output.contains("out-1 不可用"), "{output}");
    assert!(
        output.contains("execution_path: provider_control_protocol"),
        "{output}"
    );
    assert!(output.contains("request_id: ctrl-1"), "{output}");
    assert!(output.contains("tool_use_id: toolu-1"), "{output}");
    assert!(!output.contains("活动详情 out-1"), "{output}");
    assert!(
        !output.contains("PROVIDER NATIVE OUTPUT RENDERED AFTER ALLOW"),
        "{output}"
    );
    assert!(!output.contains("Tool 输出 - stdout 已捕获"), "{output}");
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(
        !output.contains("已允许 provider-native shell tool 执行"),
        "{output}"
    );
    assert!(!output.contains("Activity details tool-1"), "{output}");
    assert!(
        !output.contains("run_shell_command requested: $ printf 'provider-shell-handoff"),
        "{output}"
    );
    assert!(!output.contains("Activity details out-1"), "{output}");
    assert!(
        !output.contains("Tool output - stdout captured; [Details] out-1"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
    assert_no_migrated_english_ui_labels(&output, PROVIDER_NATIVE_ZH_FORBIDDEN_UI);
    assert_no_migrated_english_ui_labels(&output, DETAILS_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_claude_without_host_executed_capability_uses_foreground_recovery() {
    let home = temp_shell_home("claude-provider-native-shell");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let claude_path = bin_dir.join("claude");
    write_executable(
        &claude_path,
        r#"#!/bin/sh
case "$*" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-claude-native-fallback","model":"claude-test"}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-claude-native-fallback","message":{"content":[{"type":"text","text":"Claude foreground recovery received shell evidence."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-claude-native-fallback","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-claude-native-fallback","model":"claude-test"}'
read -r user_message
case "$user_message" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-claude-native-fallback","message":{"content":[{"type":"text","text":"Claude foreground recovery received shell evidence."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-claude-native-fallback","is_error":false,"result":"done"}'
    exit 0
    ;;
  *claude-provider-native-fallback*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-claude-shell","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo CLAUDE_NATIVE"},"tool_use_id":"toolu-claude-shell"}}'
    if IFS= read -r response; then
      case "$response" in
        *'"request_id":"ctrl-claude-shell"'*'"behavior":"allow"'*CLAUDE_NATIVE*)
          printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-claude-native-fallback","is_error":true,"result":"unexpected provider-native allow"}'
          exit 1
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-claude-native-fallback","is_error":true,"result":"missing claude allow response"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-claude-native-fallback","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "claude",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? claude-provider-native-fallback\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("Claude foreground recovery received shell evidence."),
        "{output}"
    );
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(!output.contains("host_executed_shell"), "{output}");
    let visible = strip_ansi_escape(&output);
    assert!(
        visible
            .split(['\r', '\n'])
            .any(|line| line == "CLAUDE_NATIVE"),
        "{output}"
    );
    assert_eq!(
        count_occurrences(&visible, "cosh-osc$ echo CLAUDE_NATIVE"),
        1,
        "{output}"
    );
    assert!(
        !output.contains("missing claude allow response"),
        "{output}"
    );
}

#[test]
fn raw_cli_obvious_tty_provider_shell_permission_uses_foreground_recovery() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider tty shell\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (
                b"/details handoff-1\nexit 0\n".to_vec(),
                Duration::from_millis(4_000),
            ),
        ],
    );

    assert!(output.contains("Mode set to auto."), "{output}");
    assert!(output.contains("medium risk"), "{output}");
    assert!(!output.contains("high risk"), "{output}");
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ ssh -V"), "{output}");
    assert!(
        output.contains("execution_path: foreground_shell_pty"),
        "{output}"
    );
    assert!(
        output.contains("selected_shell_execution_path: foreground_shell_handoff_recovery"),
        "{output}"
    );
    assert!(
        output.contains("provider_result_delivery_status: provider_run_not_active"),
        "{output}"
    );
    assert!(
        output.contains("recovery_reason: provider run was not active when shell completed"),
        "{output}"
    );
    assert!(output.contains("output_id: terminal-output://"), "{output}");
    assert!(!output.contains("output_ref:"), "{output}");
    assert!(!output.contains("/output-refs/"), "{output}");
    assert!(
        !output.contains("PROVIDER TTY OUTPUT SHOULD NOT RENDER AFTER RECOVERY"),
        "{output}"
    );
}

#[test]
fn raw_cli_debug_mode_keeps_control_shell_output_foreground_owned() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_DEBUG", "1")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider auto safe shell\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );

    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(output.contains("Shell requested: $ df -h"), "{output}");
    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem"), "{output}");
    assert!(
        !output.contains("PROVIDER AUTO NATIVE OUTPUT RENDERED AFTER ALLOW"),
        "{output}"
    );
    assert!(!output.contains("Tool output: stdout captured"), "{output}");
}
