use super::*;

#[test]
fn raw_cli_provider_tool_interactive_escape_hatch_requires_explicit_send() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"?? provider interactive failure\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"/details out-1\n/details tool-2\n".to_vec(),
                Duration::from_millis(2_500),
            ),
            (
                b"/send-to-shell handoff-1\n".to_vec(),
                Duration::from_millis(1_000),
            ),
            (
                b"echo after-handoff\n".to_vec(),
                Duration::from_millis(2_000),
            ),
            (
                b"/details handoff-1\nexit\n".to_vec(),
                Duration::from_millis(1_000),
            ),
        ],
    );

    assert!(output.contains("may require foreground shell"), "{output}");
    assert!(output.contains("[Send to shell]"), "{output}");
    assert!(output.contains("handoff-1"), "{output}");
    assert!(
        output.contains("Tool error: sudo: a terminal is required")
            || output.contains("Tool - error · sudo: a terminal is required")
            || output.contains("Tool - error"),
        "{output}"
    );
    assert!(output.contains("sudo: a terminal is required"), "{output}");
    assert!(output.contains("Sending to shell"), "{output}");
    assert!(output.contains("$ git status"), "{output}");
    assert!(output.contains("Activity details handoff-1"), "{output}");
    assert!(
        output.contains("evidence: ShellCommandCompleted"),
        "{output}"
    );
    assert!(
        output.contains("execution_path: foreground_shell_pty"),
        "{output}"
    );
    assert!(output.contains("preview_hash: fnv1a64:"), "{output}");
    assert!(output.contains("actor: user"), "{output}");
    assert!(output.contains("source: send_to_shell"), "{output}");
    assert!(output.contains("tool_use_id: toolu-tty"), "{output}");
    assert!(output.contains("redaction_status: ref_only"), "{output}");
    assert!(
        output.contains(
            "recovery_reason: no provider request id; shell evidence continuation required"
        ) || output.contains("Command result analysis for handoff-1"),
        "{output}"
    );
    assert!(output.contains("after-handoff"), "{output}");
    assert!(!output.contains("Tool result for request"), "{output}");
    assert!(!output.contains("Governance:"), "{output}");
}

#[test]
fn raw_cli_provider_tool_send_to_shell_uses_zh_language_env() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (
                b"?? provider interactive failure\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"/details out-1\n/details tool-2\n".to_vec(),
                Duration::from_millis(2_500),
            ),
            (
                b"/send-to-shell handoff-1\n".to_vec(),
                Duration::from_millis(1_000),
            ),
            (
                b"echo after-zh-handoff\n".to_vec(),
                Duration::from_millis(2_000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );

    assert!(output.contains("可能需要前台 shell"), "{output}");
    assert!(output.contains("正在发送到 shell"), "{output}");
    assert!(
        output.contains("handoff-1 将在前台 shell 中运行。"),
        "{output}"
    );
    assert!(output.contains("$ git status"), "{output}");
    assert!(output.contains("after-zh-handoff"), "{output}");
    assert!(!output.contains("Sending to shell"), "{output}");
    assert!(
        !output.contains("will run in the foreground shell"),
        "{output}"
    );
    assert!(!output.contains("may require foreground shell"), "{output}");
    assert!(!output.contains("bash: /send-to-shell"), "{output}");
}

#[test]
fn raw_cli_missing_send_to_shell_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/send-to-shell missing-handoff\n\
         echo after-missing-handoff\n\
         exit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("Shell handoff 未找到"), "{output}");
    assert!(
        output.contains("missing-handoff 不可用；请先对 provider tool failure 使用 Details 操作"),
        "{output}"
    );
    assert!(!output.contains("Shell handoff not found"), "{output}");
    assert!(output.contains("after-missing-handoff"), "{output}");
    assert!(!output.contains("bash: /send-to-shell"), "{output}");
}

// Repro for the install-openclaw silent-stop report: an approved handoff
// whose command carries a secret (`--api-key "sk-..."`) is redacted to
// `<redacted sensitive command>` by the preexec marker, so the tracked
// closure path can never match the command block and the untracked
// fallback is vetoed by the observed CommandStarted. The
// host_executed_shell result is then never delivered and the provider
// waits forever. This test asserts the desired behavior (result
// delivered), so it FAILS on the buggy code and becomes the regression
// test once fixed.
#[test]
fn raw_cli_approved_shell_handoff_with_secret_delivers_host_executed_result() {
    let home = temp_shell_home("cosh-core-handoff-secret");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-handoff-secret","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-handoff-secret*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-handoff-secret","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"echo installed --api-key \"sk-test-cosh-repro-1234\""},"tool_use_id":"toolu-handoff-secret"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    if [ -n "$response" ]; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'"exit_code":0'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-handoff-secret","message":{"content":[{"type":"text","text":"HANDOFF SECRET HOSTEXEC RECEIVED"}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-secret","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-handoff-secret","is_error":true,"result":"missing secret host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-secret","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval trust confirm\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-handoff-secret\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    // #2142 acceptance: no durable surface may retain the plaintext secret.
    // The scripted cosh-core under bin/ is the only legitimate holder (it is
    // test fixture input, not a session artifact).
    assert!(
        home.join(".copilot-shell").is_dir(),
        "durable surfaces moved out of HOME; the leak sweep below would go blind"
    );
    let leaks = files_containing(&home, "sk-test-cosh-repro-1234", &bin_dir);
    let _ = fs::remove_dir_all(&home);
    assert!(
        leaks.is_empty(),
        "plaintext secret persisted to durable surfaces: {leaks:?}"
    );

    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("HANDOFF SECRET HOSTEXEC RECEIVED"),
        "host_executed_shell result was never delivered back to the provider \
         for a secret-bearing approved handoff: {output}"
    );
    assert!(
        !output.contains("missing secret host_executed_shell result"),
        "{output}"
    );
}

/// Recursively lists files under `root` (excluding `exclude` subtree) whose
/// bytes contain `needle` — the #2142 redaction-unchanged acceptance sweep
/// over journal / output refs / history / audit in the session HOME.
fn files_containing(
    root: &std::path::Path,
    needle: &str,
    exclude: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.starts_with(exclude) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                if bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
                {
                    hits.push(path);
                }
            }
        }
    }
    hits
}

#[test]
fn raw_cli_approved_shell_handoff_command_not_found_does_not_intercept() {
    let home = temp_shell_home("cosh-core-handoff-command-not-found");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-handoff-command-not-found","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-handoff-command-not-found*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-handoff-command-not-found","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"/help"},"tool_use_id":"toolu-handoff-command-not-found"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    if [ -n "$response" ]; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'/help'*'"exit_code":127'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-handoff-command-not-found","message":{"content":[{"type":"text","text":"HANDOFF COMMAND NOT FOUND HOSTEXEC RECEIVED"}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-command-not-found","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-handoff-command-not-found","is_error":true,"result":"missing command-not-found host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-command-not-found","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval trust confirm\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-handoff-command-not-found\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("HANDOFF COMMAND NOT FOUND HOSTEXEC RECEIVED"),
        "{output}"
    );
    assert!(!output.contains("intercepted  slash"), "{output}");
    assert!(
        !output.contains("missing command-not-found host_executed_shell result"),
        "{output}"
    );
}

#[test]
fn raw_cli_approved_shell_handoff_bypasses_marker_intercepts() {
    let home = temp_shell_home("cosh-core-handoff-bypass-marker");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-handoff-bypass-marker","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-handoff-bypass-marker*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-handoff-bypass-marker","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"?? should-run-as-shell"},"tool_use_id":"toolu-handoff-bypass-marker"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    if [ -n "$response" ]; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'?? should-run-as-shell'*'"exit_code":127'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-handoff-bypass-marker","message":{"content":[{"type":"text","text":"HANDOFF BYPASS MARKER HOSTEXEC RECEIVED"}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-bypass-marker","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-handoff-bypass-marker","is_error":true,"result":"missing marker-bypass host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-bypass-marker","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval trust confirm\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-handoff-bypass-marker\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("HANDOFF BYPASS MARKER HOSTEXEC RECEIVED"),
        "{output}"
    );
    assert!(!output.contains("agent_marker"), "{output}");
    assert!(!output.contains("intercepted"), "{output}");
    assert!(
        !output.contains("missing marker-bypass host_executed_shell result"),
        "{output}"
    );
}

#[test]
fn raw_cli_zsh_approved_shell_handoff_bypasses_marker_intercepts() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_shell_home("cosh-core-zsh-handoff-bypass-marker");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-zsh-handoff-bypass-marker","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-zsh-handoff-bypass-marker*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-zsh-handoff-bypass-marker","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"?? should-run-as-zsh-shell"},"tool_use_id":"toolu-zsh-handoff-bypass-marker"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    if [ -n "$response" ]; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'?? should-run-as-zsh-shell'*'"exit_code":127'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-zsh-handoff-bypass-marker","message":{"content":[{"type":"text","text":"ZSH HANDOFF BYPASS MARKER HOSTEXEC RECEIVED"}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-zsh-handoff-bypass-marker","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-zsh-handoff-bypass-marker","is_error":true,"result":"missing zsh marker-bypass host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-zsh-handoff-bypass-marker","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &["--shell", "zsh"],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval trust confirm\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-zsh-handoff-bypass-marker\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("ZSH HANDOFF BYPASS MARKER HOSTEXEC RECEIVED"),
        "{output}"
    );
    assert!(!output.contains("agent_marker"), "{output}");
    assert!(!output.contains("intercepted"), "{output}");
    assert!(
        !output.contains("missing zsh marker-bypass host_executed_shell result"),
        "{output}"
    );
}

#[test]
fn raw_cli_approved_shell_handoff_wrapper_does_not_leak() {
    let home = temp_shell_home("cosh-core-handoff-wrapper-leak");
    let inputrc_path = home.join(".inputrc");
    fs::write(&inputrc_path, "set enable-bracketed-paste on\n").unwrap();
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-handoff-wrapper-leak","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-handoff-wrapper-leak*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-handoff-wrapper-leak","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"printf wrapper-visible"},"tool_use_id":"toolu-handoff-wrapper-leak"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    if [ -n "$response" ]; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*'printf wrapper-visible'*'wrapper-visible'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-handoff-wrapper-leak","message":{"content":[{"type":"text","text":"HANDOFF WRAPPER LEAK HOSTEXEC RECEIVED"}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-wrapper-leak","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-handoff-wrapper-leak","is_error":true,"result":"missing wrapper-leak host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-handoff-wrapper-leak","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let inputrc_path_str = inputrc_path.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("INPUTRC", &inputrc_path_str),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_CORE_PATH", &cosh_core_path_str),
        ],
        vec![
            (b"/mode approval trust confirm\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-handoff-wrapper-leak\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("wrapper-visible"), "{output}");
    assert!(
        output.contains("HANDOFF WRAPPER LEAK HOSTEXEC RECEIVED"),
        "{output}"
    );
    let visible = strip_ansi_escape(&output).replace('\r', "");
    assert_eq!(
        count_occurrences(&visible, "\nprintf wrapper-visible\n"),
        1,
        "{output}"
    );
    assert!(!output.contains("_COSH_HANDOFF"), "{output}");
    assert!(!output.contains("_cosh_prepare_staged_handoff"), "{output}");
    assert!(!output.contains("COSH_SHELL_HANDOFF_BYPASS"), "{output}");
    let command_echo = output
        .rfind("printf wrapper-visible")
        .expect("approved command echo");
    let after_command_echo = &output[command_echo + "printf wrapper-visible".len()..];
    let command_output = after_command_echo
        .find("wrapper-visible")
        .expect("approved command output");
    assert!(
        after_command_echo[..command_output].contains("\u{1b}[?2004l"),
        "bracketed-paste disable did not reach the outer terminal between the approved command echo and its output\n{output:?}"
    );
    assert!(
        !output.contains("missing wrapper-leak host_executed_shell result"),
        "{output}"
    );
}

#[test]
fn raw_cli_send_to_shell_command_triggers_trusted_project_hook() {
    let fixture = temp_shell_home("send-to-shell-project-hook");
    let project = fixture.join("project");
    let hooks_dir = project.join(".cosh/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    write_executable(
        &hooks_dir.join("project-guard.sh"),
        "#!/bin/sh\n# cosh-hook: project-guard\n# match-commands: git\nprintf '{\"hook_id\":\"project-guard\",\"severity\":\"warning\",\"title\":\"PROJECT GUARD SEND TO SHELL\",\"description\":\"project hook fired\",\"suggestion\":\"review against project policy\"}'\n",
    );
    let trust_store = fixture.join("trusted-project-hooks");
    fs::write(
        &trust_store,
        format!(
            "# cosh-shell trusted project hook roots\n{}\n",
            project.display()
        ),
    )
    .unwrap();

    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[(
            "COSH_SHELL_PROJECT_TRUST_STORE",
            trust_store.to_str().unwrap(),
        )],
        &project,
        vec![
            (
                b"?? provider interactive failure\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"/send-to-shell handoff-1\n".to_vec(),
                Duration::from_millis(2_500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(output.contains("Sending to shell"), "{output}");
    assert!(output.contains("$ git status"), "{output}");
    assert!(output.contains("PROJECT GUARD SEND TO SHELL"), "{output}");
}
