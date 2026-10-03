use super::*;

#[test]
fn raw_cli_cancel_is_intercepted_and_keeps_shell_usable() {
    let output = run_raw_cli_with_input("fake", "/cancel\necho after-cancel\nexit\n");

    assert!(output.contains("Agent cancelled"));
    assert!(output.contains("no active Agent run is currently waiting for cancellation"));
    assert!(output.contains("Shell remains active."));
    assert!(output.contains("after-cancel"));
    assert!(!output.contains("bash: /cancel"));
}

#[test]
fn raw_cli_cancel_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/cancel\n\
         echo after-cancel\n\
         exit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("Agent 已取消"), "{output}");
    assert!(output.contains("当前没有等待取消的 Agent 运行"), "{output}");
    assert!(output.contains("Shell 保持可用。"), "{output}");
    assert!(output.contains("after-cancel"), "{output}");
    assert!(!output.contains("Agent cancelled"), "{output}");
    assert!(
        !output.contains("no active Agent run is currently waiting for cancellation"),
        "{output}"
    );
    assert!(!output.contains("Shell remains active."), "{output}");
    assert!(!output.contains("bash: /cancel"), "{output}");
}

#[test]
fn raw_cli_cancel_stops_active_agent_run_and_keeps_shell_usable() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? hold test slow agent\n".to_vec(), Duration::ZERO),
            (b"/cancel\n".to_vec(), Duration::from_millis(1_000)),
            (
                b"echo after-active-cancel\nexit\n".to_vec(),
                Duration::from_millis(700),
            ),
        ],
    );

    assert!(output.contains("Agent cancellation requested"));
    assert!(output.contains("Stopping active Agent run"));
    assert!(output.contains("Agent cancelled"));
    assert!(output.contains("Reason: user requested cancellation"));
    assert!(output.contains("after-active-cancel"));
    assert!(!output.contains("bash: /cancel"));
    assert!(!output.contains("Slow fake response for"));
}

#[test]
fn raw_cli_active_cancel_uses_zh_language_env() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? text then wait slow agent\n".to_vec(), Duration::ZERO),
            (b"/cancel\n".to_vec(), Duration::from_millis(700)),
            (
                b"echo after-active-cancel\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(output.contains("Agent 取消请求已发送"), "{output}");
    assert!(output.contains("正在停止 active Agent 运行..."), "{output}");
    assert!(output.contains("Agent 已取消"), "{output}");
    assert!(output.contains("原因: 用户请求取消"), "{output}");
    assert!(output.contains("after-active-cancel"), "{output}");
    assert!(!output.contains("Agent cancellation requested"), "{output}");
    assert!(!output.contains("Stopping active Agent run"), "{output}");
    assert!(
        !output.contains("Reason: user requested cancellation"),
        "{output}"
    );
    assert!(!output.contains("bash: /cancel"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_stops_active_agent_run_and_keeps_shell_usable() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? very slow agent\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(500)),
            (
                b"echo after-agent-ctrl-c\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(output.contains("Agent cancellation requested"));
    assert!(output.contains("Stopping active Agent run"));
    assert!(output.contains("Agent cancelled"));
    assert!(output.contains("Reason: user requested cancellation"));
    assert!(output.contains("after-agent-ctrl-c"));
    assert!(!output.contains("Slow fake response for"));
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(!output.contains("No provider response within"), "{output}");
    assert!(!output.contains("cosh-osc$ cosh-osc$"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_interrupts_foreground_command_without_agent_cancel() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"sleep 5\n".to_vec(), Duration::from_millis(500)),
            (vec![0x03], Duration::from_millis(500)),
            (
                b"echo after-foreground-ctrl-c\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(output.contains("sleep 5"), "{output}");
    assert!(output.contains("after-foreground-ctrl-c"), "{output}");
    assert!(!output.contains("Agent cancellation requested"), "{output}");
    assert!(
        !output.contains("Reason: user requested cancellation"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
    assert!(!output.contains("The command sleep 5 failed"), "{output}");
}

#[test]
fn raw_cli_ctrl_backslash_recovers_ignored_foreground_command() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"bash -c 'trap \"\" INT; trap \"exit 0\" QUIT; while :; do sleep 1; done'\n"
                    .to_vec(),
                Duration::ZERO,
            ),
            (vec![0x03], Duration::from_millis(500)),
            (vec![0x1c], Duration::from_millis(500)),
            (
                b"echo after-foreground-escalation\nexit\n".to_vec(),
                Duration::from_millis(1_000),
            ),
        ],
    );

    assert!(output.contains("after-foreground-escalation"), "{output}");
    assert!(!output.contains("Agent cancellation requested"), "{output}");
    assert!(
        !output.contains("Reason: user requested cancellation"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_active_agent_cancel_is_idempotent() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? very slow agent\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(500)),
            (vec![0x03], Duration::from_millis(100)),
            (
                b"echo after-double-agent-ctrl-c\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert_eq!(
        count_occurrences(&output, "Agent cancellation requested"),
        1,
        "{output}"
    );
    assert_eq!(
        count_occurrences(&output, "Stopping active Agent run"),
        1,
        "{output}"
    );
    assert_eq!(
        count_occurrences(&output, "Reason: user requested cancellation"),
        1,
        "{output}"
    );
    assert!(output.contains("after-double-agent-ctrl-c"), "{output}");
    assert!(!output.contains("Slow fake response for"), "{output}");
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(!output.contains("cosh-osc$ cosh-osc$"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_drops_unclosed_request_block_before_prompt() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"?? slow unclosed request then wait\n".to_vec(),
                Duration::ZERO,
            ),
            (vec![0x03], Duration::from_millis(500)),
            (
                b"echo after-unclosed-request-cancel\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-unclosed-request-cancel"), "{output}");
    assert!(!output.contains("```cosh-request"), "{output}");
    assert!(!output.contains("Agent Requested Evidence"), "{output}");
    assert!(
        !output.contains("Evidence history index received"),
        "{output}"
    );
    assert!(!output.contains("cosh-osc$ cosh-osc$"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_drops_late_provider_cancel_timeout_artifact() {
    let home = temp_shell_home("qwen-late-cancel-artifact");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
trap 'printf "%s\n" "{\"type\":\"result\",\"subtype\":\"error\",\"session_id\":\"sess-late-cancel\",\"is_error\":true,\"result\":\"Agent timed out: No provider response within 20s\"}"; exit 0' TERM INT HUP
read -r init || exit 0
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-late-cancel","model":"qwen-test"}'
read -r user_message || exit 0
case "$user_message" in
  *provider-cancel-artifact*)
    while :; do sleep 1; done
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-late-cancel","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"?? provider-cancel-artifact\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(700)),
            (
                b"echo after-provider-cancel\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-provider-cancel"), "{output}");
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(!output.contains("No provider response within"), "{output}");
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
}

#[test]
fn raw_cli_ctrl_c_archives_provider_cancel_artifact_in_details() {
    let home = temp_shell_home("qwen-cancel-artifact-details");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/usr/bin/env perl
use strict;
use warnings;
$| = 1;
$SIG{TERM} = sub {
  print "PROVIDER_CANCEL_STDOUT_ARTIFACT\n";
  print STDERR "PROVIDER_CANCEL_STDERR_ARTIFACT\n";
  exit 0;
};
$SIG{INT} = $SIG{TERM};
$SIG{HUP} = $SIG{TERM};
my $init = <STDIN>;
exit 0 unless defined $init;
print "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"init-1\",\"response\":{\"subtype\":\"initialize\",\"capabilities\":{\"can_handle_can_use_tool\":true,\"can_handle_host_executed_shell_tool_result\":true}}}}\n";
print "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-cancel-artifact-details\",\"model\":\"qwen-test\"}\n";
my $user_message = <STDIN>;
exit 0 unless defined $user_message;
if ($user_message =~ /provider-cancel-artifact-details/) {
  while (1) { sleep 1; }
}
print "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"sess-cancel-artifact-details\",\"is_error\":false,\"result\":\"ignored\"}\n";
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (
                b"?? provider-cancel-artifact-details\n".to_vec(),
                Duration::ZERO,
            ),
            (vec![0x03], Duration::from_millis(1_500)),
            (
                b"/details provider-cancel-1\nexit\n".to_vec(),
                Duration::from_millis(4_000),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Details: provider-cancel-1"), "{output}");
    assert!(output.contains("Provider cancel details"), "{output}");
    assert!(
        output.contains("PROVIDER_CANCEL_STDOUT_ARTIFACT"),
        "{output}"
    );
    assert!(
        output.contains("PROVIDER_CANCEL_STDERR_ARTIFACT"),
        "{output}"
    );
    let details_pos = output
        .find("Provider cancel details")
        .expect("details panel");
    assert!(
        !output[..details_pos].contains("PROVIDER_CANCEL_STDOUT_ARTIFACT"),
        "{output}"
    );
    assert!(
        !output[..details_pos].contains("PROVIDER_CANCEL_STDERR_ARTIFACT"),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_drops_late_provider_tool_request_artifact() {
    let home = temp_shell_home("qwen-late-tool-request-artifact");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
trap 'printf "%s\n" "{\"type\":\"control_request\",\"request_id\":\"late-ctrl\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"run_shell_command\",\"input\":{\"command\":\"echo SHOULD_NOT_RUN\"},\"tool_use_id\":\"late-tool\"}}"; exit 0' TERM INT HUP
read -r init || exit 0
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-late-tool","model":"qwen-test"}'
read -r user_message || exit 0
case "$user_message" in
  *provider-cancel-late-tool*)
    while :; do sleep 1; done
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-late-tool","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"?? provider-cancel-late-tool\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(700)),
            (
                b"echo after-late-tool-cancel\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-late-tool-cancel"), "{output}");
    assert!(!output.contains("SHOULD_NOT_RUN"), "{output}");
    assert!(!output.contains("late-ctrl"), "{output}");
    assert!(!output.contains("echo SHOULD_NOT_RUN"), "{output}");
    assert_no_approval_request_card(&output);
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_drops_late_provider_tool_error_artifact() {
    let home = temp_shell_home("qwen-late-tool-error-artifact");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
trap 'printf "%s\n" "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_cancel\",\"is_error\":true,\"content\":\"Tool error: Dispatcher shutdown\"}]}}"; exit 0' TERM INT HUP
read -r init || exit 0
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-late-tool-error","model":"qwen-test"}'
read -r user_message || exit 0
case "$user_message" in
  *provider-cancel-late-tool-error*)
    while :; do sleep 1; done
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-late-tool-error","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (
                b"?? provider-cancel-late-tool-error\n".to_vec(),
                Duration::ZERO,
            ),
            (vec![0x03], Duration::from_millis(700)),
            (
                b"echo after-late-tool-error-cancel\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-late-tool-error-cancel"), "{output}");
    assert!(!output.contains("Dispatcher shutdown"), "{output}");
    assert!(!output.contains("Tool error:"), "{output}");
    assert!(!output.contains("toolu_cancel"), "{output}");
    assert!(!output.contains("Using a fresh provider turn"), "{output}");
    assert!(!output.contains("failed with exit code"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_does_not_resume_cancelled_provider_session() {
    let home = temp_shell_home("qwen-cancelled-session-not-resumed");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
case " $* " in
  *" --resume cancelled-session "*)
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"cancelled-session","is_error":true,"result":"BAD_RESUME_CANCELLED_SESSION"}'
    exit 1
    ;;
esac
read -r init || exit 0
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"cancelled-session","model":"qwen-test"}'
read -r user_message || exit 0
case "$user_message" in
  *continue*)
    printf '%s\n' "$user_message" > "$HOME/second-prompt.txt"
    printf '%s\n' '{"type":"assistant","session_id":"second-session","message":{"content":[{"type":"text","text":"SECOND RUN WITH CANCEL FACTS"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"second-session","is_error":false,"result":"done"}'
    exit 0
    ;;
  *cancelled-provider-session*)
    while :; do sleep 1; done
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"cancelled-session","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"?? cancelled-provider-session\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(700)),
            (b"?? continue\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );
    let second_prompt = fs::read_to_string(home.join("second-prompt.txt")).unwrap_or_default();
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("SECOND RUN WITH CANCEL FACTS"), "{output}");
    assert!(
        second_prompt.contains("cancelled: user requested cancellation"),
        "{second_prompt}"
    );
    assert!(!output.contains("BAD_RESUME_CANCELLED_SESSION"), "{output}");
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
}

#[test]
fn raw_cli_ctrl_c_drops_late_fake_question_card() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? late card after cancel\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(300)),
            (
                b"echo after-late-card-cancel\nexit\n".to_vec(),
                Duration::from_millis(1_000),
            ),
        ],
    );

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-late-card-cancel"), "{output}");
    assert!(
        !output.contains("LATE QUESTION SHOULD NOT RENDER"),
        "{output}"
    );
    assert!(!output.contains("Agent question"), "{output}");
    assert!(!output.contains("Answer sent"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_drops_late_fake_tool_artifact() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? late artifact after cancel\n".to_vec(), Duration::ZERO),
            (vec![0x03], Duration::from_millis(300)),
            (
                b"echo after-late-artifact-cancel\nexit\n".to_vec(),
                Duration::from_millis(1_000),
            ),
        ],
    );

    assert!(output.contains("Agent cancellation requested"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-late-artifact-cancel"), "{output}");
    assert!(
        !output.contains("LATE TOOL ARTIFACT SHOULD NOT RENDER"),
        "{output}"
    );
    assert!(!output.contains("late-tool"), "{output}");
    assert!(!output.contains("Tool error:"), "{output}");
}

#[test]
fn raw_cli_ctrl_c_cancels_active_agent_without_queueing_generic_failure() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_ANALYSIS_MODE", "auto")],
        vec![
            (b"?? very slow agent\n".to_vec(), Duration::ZERO),
            (
                b"ls /path/that/does/not/exist\n".to_vec(),
                Duration::from_millis(200),
            ),
            (vec![0x03], Duration::from_millis(500)),
            (
                b"echo after-queued-cancel\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    assert!(!output.contains("Agent queued"), "{output}");
    assert!(output.contains("Agent cancelled"), "{output}");
    assert!(output.contains("after-queued-cancel"), "{output}");
    assert!(
        !output.contains("The command ls /path/that/does/not/exist failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
    assert!(!output.contains("Slow fake response for"), "{output}");
}
