use super::*;

#[test]
fn raw_cli_host_executed_shell_timeout_interrupts_and_returns_result() {
    let output = run_host_executed_shell_timeout(&[]);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ sleep 10"), "{output}");
    assert!(
        output.contains("Command exceeded configured shell handoff timeout (1s)."),
        "{output}"
    );
    assert!(
        output.contains("Sent interrupt to foreground PTY; waiting for shell evidence."),
        "{output}"
    );
    assert!(
        output.contains("Host-executed timeout interrupt result received."),
        "{output}"
    );
    assert!(output.contains("Shell: timed_out · req-1"), "{output}");
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(!output.contains("req-2"), "{output}");
    assert!(
        !output.contains("missing timeout interrupt result"),
        "{output}"
    );
}

#[test]
fn raw_cli_host_executed_password_prompt_timeout_defers_notice_until_prompt() {
    let home = temp_shell_home("qwen-host-executed-password-timeout");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("sudo"),
        r#"#!/bin/sh
prompt='[sudo] password for cosh timeout: '
while [ "$#" -gt 0 ]; do
  case "$1" in
    -p) shift; prompt="$1" ;;
  esac
  shift || true
done
printf '%s' "$prompt" >/dev/tty
IFS= read -r _password </dev/tty
exit 1
"#,
    );
    let command = format!(
        "PATH=\"{}\":$PATH sudo -p \"[sudo] password for cosh timeout: \" true",
        bin_dir.display()
    );
    let co_path = bin_dir.join("co");
    let command_json = json_string(&command);
    write_executable(
        &co_path,
        &format!(
            r#"#!/bin/sh
read -r init
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}}}}}'
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"sess-host-executed-password-timeout","model":"qwen-test"}}'
read -r user_message
case "$user_message" in
  *provider-host-executed-password-timeout*)
    printf '%s\n' '{{"type":"control_request","request_id":"ctrl-password-timeout","request":{{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{{"command":"{command_json}"}},"tool_use_id":"toolu-password-timeout"}}}}'
    if IFS= read -r response; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'"status":"timed_out"'*)
          printf '%s\n' '{{"type":"assistant","session_id":"sess-host-executed-password-timeout","message":{{"content":[{{"type":"text","text":"Host-executed password timeout result received."}}]}}}}'
          printf '%s\n' '{{"type":"result","subtype":"success","session_id":"sess-host-executed-password-timeout","is_error":false,"result":"done"}}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{{"type":"result","subtype":"error","session_id":"sess-host-executed-password-timeout","is_error":true,"result":"missing password timeout result"}}'
    exit 1
    ;;
esac
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"sess-host-executed-password-timeout","is_error":false,"result":"ignored"}}'
"#
        ),
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("PATH", path.as_str()),
            ("COSH_SHELL_HANDOFF_TIMEOUT_SECS", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-host-executed-password-timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    let prompt_pos = output.find("password for cosh timeout:").expect(&output);
    let notice_pos = output
        .find("Command exceeded configured shell handoff timeout (1s).")
        .expect(&output);
    assert!(prompt_pos < notice_pos, "{output}");
    assert!(
        output.contains("Sent interrupt to foreground PTY; waiting for shell evidence."),
        "{output}"
    );
    assert!(
        output.contains("Host-executed password timeout result received."),
        "{output}"
    );
    assert!(output.contains("Shell: timed_out · req-1"), "{output}");
    assert!(!output.contains("dummy-password"), "{output}");
    assert!(
        !output.contains("missing password timeout result"),
        "{output}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_host_executed_input_wait_timeout_returns_evidence() {
    let home = temp_shell_home("qwen-host-executed-input-wait-timeout");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("input-wait-helper"),
        r#"#!/bin/sh
trap 'printf "\nINPUT_WAIT_INTERRUPTED\n" >/dev/tty; exit 130' INT TERM HUP
printf 'Continue input-wait acceptance? ' >/dev/tty
IFS= read -r _answer </dev/tty
printf '\nINPUT_WAIT_ANSWERED\n' >/dev/tty
"#,
    );
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-host-executed-input-wait-timeout","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *provider-host-executed-input-wait-timeout*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-input-wait-timeout","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"input-wait-helper"},"tool_use_id":"toolu-input-wait-timeout"}}'
    if IFS= read -r response; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'"exit_code":130'*'"input_wait":{"detected":true'*'"interrupted":true'*'"reason":"input-wait-timeout"'*'"status":"timed_out"'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-host-executed-input-wait-timeout","message":{"content":[{"type":"text","text":"Host-executed input-wait timeout result received."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-host-executed-input-wait-timeout","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-host-executed-input-wait-timeout","is_error":true,"result":"missing input-wait timeout result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-host-executed-input-wait-timeout","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("PATH", path.as_str()),
            ("COSH_SHELL_HANDOFF_TIMEOUT_SECS", "30"),
            ("COSH_SHELL_INPUT_WAIT_TIMEOUT_SECS", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-host-executed-input-wait-timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            // This accepts the pending approval; the helper cannot own the PTY before it.
            (b"\n".to_vec(), Duration::from_secs(1)),
            // Send no answer while the helper owns the PTY; Ctrl+C only bounds failed runs.
            (b"\x03".to_vec(), Duration::from_secs(12)),
            (b"exit 0\n".to_vec(), Duration::from_secs(1)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("Continue input-wait acceptance?"),
        "{output}"
    );
    assert!(output.contains("INPUT_WAIT_INTERRUPTED"), "{output}");
    assert!(!output.contains("INPUT_WAIT_ANSWERED"), "{output}");
    assert!(
        output.contains(
            "Foreground command waited for keyboard input over 1s with no answer \
             (input_wait_timeout_secs)."
        ),
        "{output}"
    );
    assert!(
        output.contains("Interrupted the command (like Ctrl+C);"),
        "{output}"
    );
    assert!(output.contains("non-interactively."), "{output}");
    assert!(
        output.contains("Host-executed input-wait timeout result received."),
        "{output}"
    );
    assert!(output.contains("Shell: timed_out · req-1"), "{output}");
    assert!(
        !output.contains("missing input-wait timeout result"),
        "{output}"
    );
    assert!(
        !output.contains("Command exceeded configured shell handoff timeout"),
        "{output}"
    );
}

#[test]
fn raw_cli_host_executed_fullscreen_timeout_defers_notice_until_exit_alt_screen() {
    let home = temp_shell_home("qwen-host-executed-fullscreen-timeout");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fullscreen_script = bin_dir.join("fullscreen-timeout-helper");
    write_executable(
        &fullscreen_script,
        r#"#!/bin/sh
trap 'printf "\033[?1049lFULLSCREEN_DONE\n"; exit 130' INT TERM
printf '\033[?1049hFULLSCREEN_START\n'
while :; do
  sleep 1
done
"#,
    );
    let command = "fullscreen-timeout-helper";
    let co_path = bin_dir.join("co");
    let command_json = json_string(command);
    write_executable(
        &co_path,
        &format!(
            r#"#!/bin/sh
read -r init
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}}}}}'
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"sess-host-executed-fullscreen-timeout","model":"qwen-test"}}'
read -r user_message
case "$user_message" in
  *provider-host-executed-fullscreen-timeout*)
    printf '%s\n' '{{"type":"control_request","request_id":"ctrl-fullscreen-timeout","request":{{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{{"command":"{command_json}"}},"tool_use_id":"toolu-fullscreen-timeout"}}}}'
    if IFS= read -r response; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'"status":"timed_out"'*)
          printf '%s\n' '{{"type":"assistant","session_id":"sess-host-executed-fullscreen-timeout","message":{{"content":[{{"type":"text","text":"Host-executed fullscreen timeout result received."}}]}}}}'
          printf '%s\n' '{{"type":"result","subtype":"success","session_id":"sess-host-executed-fullscreen-timeout","is_error":false,"result":"done"}}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{{"type":"result","subtype":"error","session_id":"sess-host-executed-fullscreen-timeout","is_error":true,"result":"missing fullscreen timeout result"}}'
    exit 1
    ;;
esac
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"sess-host-executed-fullscreen-timeout","is_error":false,"result":"ignored"}}'
"#
        ),
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("PATH", path.as_str()),
            ("COSH_SHELL_HANDOFF_TIMEOUT_SECS", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-host-executed-fullscreen-timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_secs(5)),
            (b"exit 0\n".to_vec(), Duration::from_secs(8)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    let enter_alt_pos = output.find("\x1b[?1049h").expect(&output);
    let leave_alt_pos = output.find("\x1b[?1049l").expect(&output);
    let notice_pos = output
        .find("Command exceeded configured shell handoff timeout (1s).")
        .expect(&output);
    assert!(enter_alt_pos < leave_alt_pos, "{output}");
    assert!(leave_alt_pos < notice_pos, "{output}");
    assert!(
        output.contains("Host-executed fullscreen timeout result received."),
        "{output}"
    );
    assert!(output.contains("Shell: timed_out · req-1"), "{output}");
    assert!(
        !output.contains("missing fullscreen timeout result"),
        "{output}"
    );
}

#[test]
fn raw_cli_host_executed_shell_timeout_uses_zh_language_env() {
    let output = run_host_executed_shell_timeout(&[("COSH_SHELL_LANG", "zh-CN")]);

    assert!(output.contains("已批准 req-1"), "{output}");
    assert!(output.contains("Bash tool 已发送到 shell"), "{output}");
    assert!(output.contains("$ sleep 10"), "{output}");
    assert!(
        output.contains("命令超过了配置的 shell handoff 超时时间（1s）。"),
        "{output}"
    );
    assert!(
        output.contains("已向前台 PTY 发送中断；正在等待 shell evidence。"),
        "{output}"
    );
    assert!(
        output.contains("Host-executed timeout interrupt result received."),
        "{output}"
    );
    assert!(output.contains("Shell: timed_out · req-1"), "{output}");
    assert!(!output.contains("req-2"), "{output}");
    assert!(!output.contains("Shell recovery"), "{output}");
    assert!(
        !output.contains("Command exceeded configured shell handoff timeout"),
        "{output}"
    );
    assert!(
        !output.contains("Sent interrupt to foreground PTY"),
        "{output}"
    );
    assert!(
        !output.contains("missing timeout interrupt result"),
        "{output}"
    );
}

fn run_host_executed_shell_timeout(extra_env: &[(&str, &str)]) -> String {
    let home = temp_shell_home("qwen-host-executed-timeout");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-host-executed-timeout","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *provider-host-executed-timeout*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-timeout","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"sleep 10"},"tool_use_id":"toolu-timeout"}}'
    if IFS= read -r response; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'sleep 10'*'"status":"timed_out"'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-host-executed-timeout","message":{"content":[{"type":"text","text":"Host-executed timeout interrupt result received."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-host-executed-timeout","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-host-executed-timeout","is_error":true,"result":"missing timeout interrupt result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-host-executed-timeout","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let mut env = vec![
        ("HOME", home_str.as_str()),
        ("PATH", path.as_str()),
        ("COSH_SHELL_HANDOFF_TIMEOUT_SECS", "1"),
    ];
    env.extend_from_slice(extra_env);
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &env,
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-host-executed-timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    output
}
