use super::*;

#[test]
fn raw_cli_shell_handoff_continuation_denies_second_shell_tool() {
    let (output, _prompt) = run_qwen_continuation_deny("qwen-continuation-deny", &[]);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ ssh -V"), "{output}");
    assert!(
        output.contains("Continuation summarized existing shell evidence in plan mode."),
        "{output}"
    );
    assert!(!output.contains("$ du -sh ~"), "{output}");
    assert_eq!(count_approval_prompts(&output), 1, "{output}");
}

#[test]
fn raw_cli_shell_handoff_continuation_prompt_keeps_user_mode() {
    let (output, prompt) = run_qwen_continuation_deny("qwen-continuation-prompt", &[]);

    assert!(
        output.contains("Continuation summarized existing shell evidence in plan mode."),
        "{output}"
    );
    assert!(
        prompt.contains("analysis-only continuation after a foreground shell handoff"),
        "{prompt}"
    );
    assert!(
        prompt.contains("approval mode is auto and has not changed"),
        "{prompt}"
    );
    assert!(prompt.contains("applies only to this turn"), "{prompt}");
    assert!(!prompt.contains("recommend mode"), "{prompt}");
    assert!(!prompt.contains("mode: RecommendOnly"), "{prompt}");
    assert!(prompt.contains("ShellCommandCompleted"), "{prompt}");
    assert!(
        prompt.contains("analysis-only continuation after foreground shell handoff"),
        "{prompt}"
    );
}

#[test]
fn raw_cli_zh_shell_handoff_continuation_denies_second_shell_tool() {
    let (output, _prompt) =
        run_qwen_continuation_deny("qwen-continuation-deny-zh", &[("COSH_SHELL_LANG", "zh-CN")]);

    assert!(output.contains("已批准 req-1"), "{output}");
    assert!(output.contains("Bash tool 已发送到 shell"), "{output}");
    assert!(output.contains("$ ssh -V"), "{output}");
    assert!(
        output.contains("Continuation summarized existing shell evidence in plan mode."),
        "{output}"
    );
    assert!(!output.contains("$ du -sh ~"), "{output}");
    assert_eq!(count_zh_approval_prompts(&output), 1, "{output}");
    assert!(!output.contains("Approval required"), "{output}");
    assert!(!output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
}

fn run_qwen_continuation_deny(label: &str, extra_env: &[(&str, &str)]) -> (String, String) {
    let home = temp_shell_home(label);
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
session="sess-cosh-continuation-deny"
case "$*" in
  *ShellCommandCompleted*)
    printf '%s' "$*" >"$HOME/continuation-prompt.txt"
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-continuation-deny","model":"qwen-test"}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-continuation-deny","message":{"content":[{"type":"text","text":"Continuation summarized existing shell evidence in plan mode."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-continuation-deny","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-continuation-deny","model":"qwen-test"}'
while IFS= read -r line; do
  case "$line" in
    *ShellCommandCompleted*)
      printf '%s\n' '{"type":"control_request","request_id":"ctrl-next","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"du -sh ~"},"tool_use_id":"toolu-next"}}'
      if IFS= read -r response; then
        case "$response" in
          *'"behavior":"deny"'*)
            printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-continuation-deny","message":{"content":[{"type":"text","text":"Continuation summarized existing shell evidence after tool denial."}]}}'
            printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-continuation-deny","is_error":false,"result":"done"}'
            exit 0
            ;;
        esac
      fi
      exit 2
      ;;
    *provider-auto-second-tool*)
      printf '%s\n' '{"type":"control_request","request_id":"ctrl-1","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"ssh -V"},"tool_use_id":"toolu-1"}}'
      sleep 30
      exit 0
      ;;
  esac
done
exit 0
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let mut env = vec![
        ("HOME", home_str.as_str()),
        ("PATH", path.as_str()),
        ("COSH_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS", "1"),
    ];
    env.extend_from_slice(extra_env);
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &env,
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-auto-second-tool\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit 0\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let continuation_prompt =
        fs::read_to_string(home.join("continuation-prompt.txt")).unwrap_or_default();
    let _ = fs::remove_dir_all(&home);

    (output, continuation_prompt)
}
