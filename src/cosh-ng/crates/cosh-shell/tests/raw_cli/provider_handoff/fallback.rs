use super::*;

#[test]
fn raw_cli_qwen_shell_without_advertised_host_capability_uses_foreground_shell() {
    let home = temp_shell_home("qwen-silent-resume-fallback");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
case "$*" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-silent-resume","model":"qwen-test"}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-silent-resume","message":{"content":[{"type":"text","text":"Fresh continuation summarized shell evidence after silent resume."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-silent-resume","is_error":false,"result":"done"}'
    exit 0
    ;;
esac

case " $* " in
  *" --resume "*)
    sleep 30
    exit 0
    ;;
esac

printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-silent-resume","model":"qwen-test"}'
while IFS= read -r line; do
  case "$line" in
    *ShellCommandCompleted*)
      printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-silent-resume","message":{"content":[{"type":"text","text":"Fresh continuation summarized shell evidence after silent resume."}]}}'
      printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-silent-resume","is_error":false,"result":"done"}'
      exit 0
      ;;
	    *provider-real-resume-silent*)
	      printf '%s\n' '{"type":"control_request","request_id":"ctrl-1","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"df -h"},"tool_use_id":"toolu-1"}}'
	      if IFS= read -r response; then
	        case "$response" in
		          *'"request_id":"ctrl-1"'*'"behavior":"host_executed_shell"'*)
		            printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-silent-resume","message":{"content":[{"type":"text","text":"Qwen consumed foreground shell evidence."}]}}'
		            printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-silent-resume","is_error":false,"result":"done"}'
		            exit 0
		            ;;
	        esac
	      fi
		      printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-silent-resume","is_error":true,"result":"missing host_executed_shell result"}'
		      exit 1
		      ;;
  esac
done
exit 0
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("COSH_AGENT_START_TIMEOUT_SECS", "2"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-real-resume-silent\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("Qwen consumed foreground shell evidence."),
        "{output}"
    );
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
    assert!(
        !output.contains("Agent timed out: No provider response"),
        "{output}"
    );
}

#[test]
fn raw_cli_qwen_control_shell_result_uses_foreground_transcript() {
    let home = temp_shell_home("qwen-foreground-tool-result");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-qwen-foreground-tool-result","model":"qwen-test"}'
while IFS= read -r line; do
  case "$line" in
    *qwen-foreground-tool-result*)
      printf '%s\n' '{"type":"control_request","request_id":"ctrl-1","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"df -h"},"tool_use_id":"toolu-1"}}'
      if IFS= read -r response; then
        case "$response" in
          *'"request_id":"ctrl-1"'*'"behavior":"host_executed_shell"'*)
	            printf '%s\n' '{"type":"assistant","session_id":"sess-qwen-foreground-tool-result","message":{"content":[{"type":"text","text":"Qwen saw foreground shell output."}]}}'
	            printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-qwen-foreground-tool-result","is_error":false,"result":"done"}'
            exit 0
            ;;
        esac
      fi
	      printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-qwen-foreground-tool-result","is_error":true,"result":"missing host_executed_shell result"}'
	      exit 1
      ;;
  esac
done
exit 0
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
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? qwen-foreground-tool-result\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem"), "{output}");
    assert!(
        output.contains("Qwen saw foreground shell output."),
        "{output}"
    );
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(
        !output.contains("Tool output: stdout captured; [Details]"),
        "{output}"
    );
    assert!(
        !output.contains("missing host_executed_shell result"),
        "{output}"
    );
}

#[test]
fn raw_cli_cwd_scoped_qwen_shell_uses_foreground_without_half_open_resume() {
    let home = temp_shell_home("qwen-cwd-resume-fallback");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
case "$*" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-cwd-resume","model":"qwen-test"}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-cwd-resume","message":{"content":[{"type":"text","text":"Cwd-scoped fresh continuation summarized shell evidence."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-cwd-resume","is_error":false,"result":"done"}'
    exit 0
    ;;
esac

case " $* " in
  *" --resume "*)
    sleep 30
    exit 0
    ;;
esac

printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-cwd-resume","model":"qwen-test"}'
while IFS= read -r line; do
  case "$line" in
	    *provider-cwd-resume-silent*)
	      printf '%s\n' '{"type":"control_request","request_id":"ctrl-1","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"df -h"},"tool_use_id":"toolu-1"}}'
	      if IFS= read -r response; then
	        case "$response" in
		          *'"request_id":"ctrl-1"'*'"behavior":"host_executed_shell"'*)
		            printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-cwd-resume","message":{"content":[{"type":"text","text":"Cwd-scoped foreground shell evidence handled safe shell."}]}}'
		            printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-cwd-resume","is_error":false,"result":"done"}'
		            exit 0
	            ;;
	        esac
	      fi
		      printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-cwd-resume","is_error":true,"result":"missing host_executed_shell result"}'
		      exit 1
	      ;;
  esac
done
exit 0
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("COSH_AGENT_START_TIMEOUT_SECS", "2"),
        ],
        &home,
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-cwd-resume-silent\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("Cwd-scoped foreground shell evidence handled safe shell."),
        "{output}"
    );
    assert!(
        !output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
    assert!(
        !output.contains("Agent timed out: No provider response"),
        "{output}"
    );
}
