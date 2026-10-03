use super::*;

#[test]
fn raw_cli_cosh_core_shell_evidence_tool_lists_and_reads_current_ledger() {
    let home = temp_shell_home("cosh-core-shell-evidence-tool");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-tool","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-evidence-tool-contract*)
    printf '%s\n' '{"type":"control_request","request_id":"evidence-list-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-evidence-list","action":"list_commands","limit":2}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'output_id: terminal-output://raw-session-'*'/cmd-1'*'output_available: true'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-tool","is_error":true,"result":"missing shell_evidence command index"}'; exit 1 ;;
    esac
    output_tail=${response1#*output_id: }
    output_id=${output_tail%%\\n*}
    case "$output_id" in
      terminal-output://raw-session-*/cmd-1) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-tool","is_error":true,"result":"invalid shell evidence output id"}'; exit 1 ;;
    esac
    printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"evidence-read-1\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-evidence-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":2}}"
    IFS= read -r response2 || exit 2
    case "$response2" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'action: read_output'*'direction: tail'*'lines_requested: 2'*'bounded_output_excerpt:'*'beta'*'gamma'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-tool","is_error":true,"result":"missing shell_evidence output excerpt"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-tool","message":{"content":[{"type":"text","text":"CONTROL EVIDENCE TOOL FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-tool","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-tool","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"printf 'alpha\\nbeta\\ngamma\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? cosh-core-evidence-tool-contract\n".to_vec(),
                Duration::from_secs(1),
            ),
            (
                b"/details evidence-1\n/details evidence-2\n".to_vec(),
                Duration::from_millis(3_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("alpha"), "{output}");
    assert!(output.contains("beta"), "{output}");
    assert!(output.contains("gamma"), "{output}");
    assert!(output.contains("CONTROL EVIDENCE TOOL FINAL"), "{output}");
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("Activity details evidence-2"), "{output}");
    assert!(output.contains("Original: cosh_shell_evidence"), "{output}");
    assert!(
        output.contains("Classification: shell-evidence"),
        "{output}"
    );
    assert!(output.contains("action: list_commands"), "{output}");
    assert!(output.contains("action: read_output"), "{output}");
    assert!(
        output.contains("output_id: terminal-output://raw-session-"),
        "{output}"
    );
    assert!(output.contains("direction: tail"), "{output}");
    assert!(output.contains("lines: 2"), "{output}");
    assert!(
        output.contains("Headline: command history delivered to Agent"),
        "{output}"
    );
    assert!(output.contains("1 command"), "{output}");
    assert!(
        output.contains("Headline: shell output excerpt delivered to Agent"),
        "{output}"
    );
    assert!(!output.contains("Agent Requested Evidence"), "{output}");
    assert!(!output.contains("```cosh-request"), "{output}");
    assert!(!output.contains("/output-refs/"), "{output}");
    assert!(
        !output.contains("missing shell_evidence command index"),
        "{output}"
    );
    assert!(
        !output.contains("missing shell_evidence output excerpt"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_shell_evidence_splits_agent_cards() {
    let home = temp_shell_home("cosh-core-shell-evidence-agent-split");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-agent-split","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-evidence-agent-split*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-agent-split","message":{"content":[{"type":"text","text":"PRE EVIDENCE TEXT"}]}}'
    printf '%s\n' '{"type":"control_request","request_id":"evidence-list-split","request":{"subtype":"shell_evidence","tool_use_id":"toolu-evidence-list-split","action":"list_commands","limit":1}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-agent-split","is_error":true,"result":"missing shell evidence split response"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-agent-split","message":{"content":[{"type":"text","text":"POST EVIDENCE TEXT"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-agent-split","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-agent-split","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("TERM", "xterm-256color"),
        ],
        vec![
            (
                b"printf 'split evidence ledger\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? cosh-core-evidence-agent-split\n".to_vec(),
                Duration::from_millis(800),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let pre = output.find("PRE EVIDENCE TEXT").expect(&output);
    let first_bottom = output[pre..]
        .find('╰')
        .map(|offset| pre + offset)
        .expect(&output);
    let card = output.find("Shell evidence completed").expect(&output);
    let post = output.find("POST EVIDENCE TEXT").expect(&output);
    let second_agent = output[card..post]
        .find("╭ Agent")
        .map(|offset| card + offset)
        .expect(&output);

    assert!(
        pre < first_bottom && first_bottom < card && card < second_agent && second_agent < post,
        "{output}"
    );
    assert!(
        !output.contains("missing shell evidence split response"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_read_after_truncated_host_executed_returns_output() {
    let home = temp_shell_home("cosh-core-shell-evidence-duplicate-read");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-duplicate-read","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"ctrl-duplicate-read","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"i=1; while [ $i -le 120 ]; do printf \"duplicate-read-line-%03d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n\" \"$i\"; i=$((i+1)); done"},"tool_use_id":"toolu-duplicate-read"}}'
# #1940: skip approval receipts that now precede the decision line.
while IFS= read -r response1; do
  case "$response1" in
    *'"type":"approval_receipt"'*) continue ;;
    *) break ;;
  esac
done
case "$response1" in
  *'"behavior":"host_executed_shell"'*'ShellCommandCompleted evidence'*'output_id: terminal-output://raw-session-'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-duplicate-read","is_error":true,"result":"missing host-executed output id"}'; exit 1 ;;
esac
output_tail=${response1#*output_id: }
output_id=${output_tail%%\\n*}
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"evidence-duplicate-read\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-evidence-duplicate-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":120}}"
IFS= read -r response2 || exit 2
case "$response2" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'action: read_output'*'bounded_output_excerpt:'*'duplicate-read-line-120'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-duplicate-read","is_error":true,"result":"truncated host-executed read did not return output body"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-duplicate-read","message":{"content":[{"type":"text","text":"TRUNCATED HOST EXECUTED READ FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-duplicate-read","is_error":false,"result":"done"}'
exit 0
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
                b"?? cosh-core-evidence-duplicate-read\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"/details evidence-1\n".to_vec(),
                Duration::from_millis(6_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("TRUNCATED HOST EXECUTED READ FINAL"),
        "{output}"
    );
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("Shell evidence completed"), "{output}");
    assert!(
        output.contains("shell output excerpt delivered to Agent"),
        "{output}"
    );
    assert!(output.contains("duplicate-read-line-120"), "{output}");
    assert!(output.contains("Status: success"), "{output}");
    assert!(
        !output.contains("truncated host-executed read did not return output body"),
        "{output}"
    );
    assert!(
        !output.contains("missing host-executed output id"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_control_shell_evidence_suppresses_provider_snapshot() {
    let home = temp_shell_home("cosh-core-shell-evidence-control-suppresses-snapshot");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-control-evidence-snapshot","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"snapshot-list","request":{"subtype":"shell_evidence","tool_use_id":"toolu-snapshot-list","action":"list_commands","limit":2}}'
IFS= read -r response1 || exit 2
case "$response1" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'output_id: terminal-output://raw-session-'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-control-evidence-snapshot","is_error":true,"result":"missing snapshot command index"}'; exit 1 ;;
esac
output_tail=${response1#*output_id: }
output_id=${output_tail%%\\n*}
printf '%s\n' "{\"type\":\"assistant\",\"session_id\":\"sess-cosh-core-control-evidence-snapshot\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu-snapshot-read\",\"name\":\"cosh_shell_evidence\",\"input\":{\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":20}}]}}"
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"snapshot-read\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-snapshot-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":20}}"
IFS= read -r response2 || exit 2
case "$response2" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'excerpt_status: available'*'snapshot-duplicate-line'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-control-evidence-snapshot","is_error":true,"result":"missing snapshot read response"}'; exit 1 ;;
esac
printf '%s\n' "{\"type\":\"user\",\"session_id\":\"sess-cosh-core-control-evidence-snapshot\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu-snapshot-read\",\"content\":\"ShellEvidenceExcerpt\\naction: read_output\\nexcerpt_status: available\\nbounded_output_excerpt:\\nsnapshot-duplicate-line\",\"is_error\":false}]}}"
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-control-evidence-snapshot","message":{"content":[{"type":"text","text":"SNAPSHOT SUPPRESSED FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-control-evidence-snapshot","is_error":false,"result":"done"}'
exit 0
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"printf 'snapshot-duplicate-line\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? cosh-core-control-evidence-snapshot\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("SNAPSHOT SUPPRESSED FINAL"), "{output}");
    assert_eq!(
        output.matches("Shell evidence completed").count(),
        2,
        "{output}"
    );
    assert!(
        output.contains("#cmd-1 $ printf 'snapshot-duplicate-line"),
        "{output}"
    );
    assert!(
        !output.contains("Shell evidence completed\nshell output excerpt\n"),
        "{output}"
    );
    assert!(
        !output.contains("missing snapshot command index"),
        "{output}"
    );
    assert!(
        !output.contains("missing snapshot read response"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_bypass_recent_filter_reads_after_host_executed() {
    let home = temp_shell_home("cosh-core-shell-evidence-bypass-read");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-bypass-read","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"ctrl-bypass-read","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"printf '\''bypass-one\\nbypass-two\\nbypass-three\\n'\''"},"tool_use_id":"toolu-bypass-read"}}'
# #1940: skip approval receipts that now precede the decision line.
while IFS= read -r response1; do
  case "$response1" in
    *'"type":"approval_receipt"'*) continue ;;
    *) break ;;
  esac
done
case "$response1" in
  *'"behavior":"host_executed_shell"'*'ShellCommandCompleted evidence'*'output_id: terminal-output://raw-session-'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-bypass-read","is_error":true,"result":"missing bypass host-executed output id"}'; exit 1 ;;
esac
output_tail=${response1#*output_id: }
output_id=${output_tail%%\\n*}
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"evidence-bypass-read\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-evidence-bypass-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":2,\"bypass_recent_filter\":true}}"
IFS= read -r response2 || exit 2
case "$response2" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'excerpt_status: available'*'bounded_output_excerpt:'*'bypass-two'*'bypass-three'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-bypass-read","is_error":true,"result":"bypass read did not return bounded excerpt"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-bypass-read","message":{"content":[{"type":"text","text":"BYPASS READ RETURNED EXCERPT FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-bypass-read","is_error":false,"result":"done"}'
exit 0
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
                b"?? cosh-core-evidence-bypass-read\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"/details evidence-1\n".to_vec(),
                Duration::from_millis(6_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("BYPASS READ RETURNED EXCERPT FINAL"),
        "{output}"
    );
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("Shell evidence completed"), "{output}");
    assert!(output.contains("Status: success"), "{output}");
    assert!(
        output.contains("Headline: shell output excerpt delivered to Agent"),
        "{output}"
    );
    assert!(output.contains("bypass-two"), "{output}");
    assert!(output.contains("bypass-three"), "{output}");
    assert!(
        !output.contains("bypass read did not return bounded excerpt"),
        "{output}"
    );
    assert!(
        !output.contains("missing bypass host-executed output id"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_shell_evidence_home_path_redaction_still_delivers() {
    let home = temp_shell_home("cosh-core-shell-evidence-home-path-redaction");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-home-path-redaction","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"home-path-list","request":{"subtype":"shell_evidence","tool_use_id":"toolu-home-path-list","action":"list_commands","limit":3}}'
IFS= read -r response1 || exit 2
case "$response1" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'output_id: terminal-output://raw-session-'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-home-path-redaction","is_error":true,"result":"missing home-path command index"}'; exit 1 ;;
esac
output_tail=${response1#*output_id: }
output_id=${output_tail%%\\n*}
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"home-path-read\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-home-path-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":5}}"
IFS= read -r response2 || exit 2
case "$response2" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'excerpt_status: available'*'redaction_status: excerpt_redacted'*'~/Applications/Codex.app/Contents/MacOS/Codex'*)
    case "$response2" in
      *redacted_confirmation_required*) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-home-path-redaction","is_error":true,"result":"home path redaction blocked shell evidence"}'; exit 1 ;;
    esac
    ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-home-path-redaction","is_error":true,"result":"home path shell evidence was not delivered"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-home-path-redaction","message":{"content":[{"type":"text","text":"HOME PATH EVIDENCE DELIVERED FINAL ~/Applications/Codex.app"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-home-path-redaction","is_error":false,"result":"done"}'
exit 0
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let command = format!(
        "printf 'USER PID COMMAND\\nme 123 {}/Applications/Codex.app/Contents/MacOS/Codex\\n'\n",
        home_str
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (command.into_bytes(), Duration::ZERO),
            (
                b"?? cosh-core-home-path-redaction\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"/details evidence-2\n".to_vec(),
                Duration::from_millis(4_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("HOME PATH EVIDENCE DELIVERED FINAL"),
        "{output}"
    );
    assert!(output.contains("Shell evidence completed"), "{output}");
    assert!(
        output.contains("Headline: shell output excerpt delivered to Agent"),
        "{output}"
    );
    assert!(output.contains("~/Applications/Codex.app"), "{output}");
    assert!(
        !output.contains("home path redaction blocked shell evidence"),
        "{output}"
    );
    assert!(
        !output.contains("home path shell evidence was not delivered"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_duplicate_shell_evidence_read_is_marked_duplicate() {
    let home = temp_shell_home("cosh-core-shell-evidence-duplicate-provider-read");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-duplicate-provider-read","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"duplicate-provider-list","request":{"subtype":"shell_evidence","tool_use_id":"toolu-duplicate-provider-list","action":"list_commands","limit":3}}'
IFS= read -r response1 || exit 2
case "$response1" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'output_id: terminal-output://raw-session-'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-duplicate-provider-read","is_error":true,"result":"missing duplicate-provider command index"}'; exit 1 ;;
esac
output_tail=${response1#*output_id: }
output_id=${output_tail%%\\n*}
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"duplicate-provider-read-1\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-duplicate-provider-read-1\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":5}}"
IFS= read -r response2 || exit 2
case "$response2" in
  *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'excerpt_status: available'*'duplicate-provider-line'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-duplicate-provider-read","is_error":true,"result":"first duplicate-provider read was not delivered"}'; exit 1 ;;
esac
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"duplicate-provider-read-2\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-duplicate-provider-read-2\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":5,\"bypass_recent_filter\":true}}"
IFS= read -r response3 || exit 2
case "$response3" in
  *'"behavior":"shell_evidence"'*'excerpt_status: already_delivered'*'already_delivered_recent_shell_tool_output'*)
    case "$response3" in
      *'bounded_output_excerpt:'*|*'duplicate-provider-line'*) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-duplicate-provider-read","is_error":true,"result":"second duplicate-provider read returned output body"}'; exit 1 ;;
    esac
    ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-duplicate-provider-read","is_error":true,"result":"second duplicate-provider read was not recognized"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-duplicate-provider-read","message":{"content":[{"type":"text","text":"DUPLICATE PROVIDER READ FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-duplicate-provider-read","is_error":false,"result":"done"}'
exit 0
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"printf 'duplicate-provider-line\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? cosh-core-duplicate-provider-read\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("DUPLICATE PROVIDER READ FINAL"), "{output}");
    assert!(output.contains("Shell evidence completed"), "{output}");
    assert!(
        output.contains("Shell evidence duplicate request"),
        "{output}"
    );
    assert!(
        output.contains("provider repeated the same shell evidence request"),
        "{output}"
    );
    assert!(
        output.contains("$ printf 'duplicate-provider-line"),
        "{output}"
    );
    assert!(
        !output.contains("first duplicate-provider read was not delivered"),
        "{output}"
    );
    assert!(
        !output.contains("second duplicate-provider read was not recognized"),
        "{output}"
    );
    assert!(
        !output.contains("second duplicate-provider read returned output body"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_same_command_text_reads_show_command_ids() {
    let home = temp_shell_home("cosh-core-shell-evidence-same-command-text");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-same-command-text","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"same-command-list","request":{"subtype":"shell_evidence","tool_use_id":"toolu-same-command-list","action":"list_commands","limit":5}}'
IFS= read -r response1 || exit 2
ids=$(python3 - "$response1" <<'PY'
import json, re, sys
line = sys.argv[1]
try:
    obj = json.loads(line)
    payload = obj.get("response", {}).get("response", "")
    text = payload if isinstance(payload, str) else json.dumps(payload, ensure_ascii=False)
except Exception:
    text = line
for output_id in re.findall(r"output_id: (terminal-output://\S+)", text)[:2]:
    print(output_id.split("\\n", 1)[0].rstrip('",}'))
PY
)
output_id_1=$(printf '%s\n' "$ids" | sed -n '1p')
output_id_2=$(printf '%s\n' "$ids" | sed -n '2p')
if [ -z "$output_id_1" ] || [ -z "$output_id_2" ]; then
  printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-same-command-text","is_error":true,"result":"missing same-command output ids"}'
  exit 1
fi
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"same-command-read-1\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-same-command-read-1\",\"action\":\"read_output\",\"output_id\":\"$output_id_1\",\"direction\":\"tail\",\"lines\":20}}"
IFS= read -r response2 || exit 2
case "$response2" in
  *'"behavior":"shell_evidence"'*'excerpt_status: available'*'same-command-line'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-same-command-text","is_error":true,"result":"first same-command read was not delivered"}'; exit 1 ;;
esac
printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"same-command-read-2\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-same-command-read-2\",\"action\":\"read_output\",\"output_id\":\"$output_id_2\",\"direction\":\"tail\",\"lines\":20}}"
IFS= read -r response3 || exit 2
case "$response3" in
  *'"behavior":"shell_evidence"'*'excerpt_status: available'*'same-command-line'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-same-command-text","is_error":true,"result":"second same-command read was not delivered"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-same-command-text","message":{"content":[{"type":"text","text":"SAME COMMAND TEXT FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-same-command-text","is_error":false,"result":"done"}'
exit 0
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"printf 'same-command-line\\n'\n".to_vec(), Duration::ZERO),
            (
                b"printf 'same-command-line\\n'\n".to_vec(),
                Duration::from_millis(800),
            ),
            (
                b"?? cosh-core-same-command-text\n".to_vec(),
                Duration::from_millis(1_200),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("SAME COMMAND TEXT FINAL"), "{output}");
    assert!(output.contains("Shell evidence completed"), "{output}");
    assert!(output.contains("#cmd-1 $ printf"), "{output}");
    assert!(output.contains("#cmd-2 $ printf"), "{output}");
    assert!(
        !output.contains("missing same-command output ids"),
        "{output}"
    );
    assert!(
        !output.contains("first same-command read was not delivered"),
        "{output}"
    );
    assert!(
        !output.contains("second same-command read was not delivered"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_terminal_output_read_misroute_recommends_shell_evidence_tool() {
    let home = temp_shell_home("cosh-core-shell-evidence-misroute");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-misroute","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-evidence-misroute*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-misroute","message":{"content":[{"type":"tool_use","id":"toolu-misroute","name":"read_file","input":{"path":"terminal-output://raw-session-misroute/cmd-1"}}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-misroute","message":{"content":[{"type":"text","text":"CONTROL EVIDENCE MISROUTE FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-misroute","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-misroute","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"?? cosh-core-evidence-misroute\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(3_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("CONTROL EVIDENCE MISROUTE FINAL"),
        "{output}"
    );
    assert!(output.contains("Activity details tool-1"), "{output}");
    assert!(
        output.contains("Primary: Shell output bookmark"),
        "{output}"
    );
    assert!(
        output.contains("virtual_evidence_read_misroute: true"),
        "{output}"
    );
    assert!(
        output.contains("misrouted_output_id: terminal-output://raw-session-misroute/cmd-1"),
        "{output}"
    );
    assert!(
        output.contains("recommended_action: cosh_shell_evidence_read_output"),
        "{output}"
    );
    assert!(
        !output.contains("recommended_action: fenced_cosh_request_output"),
        "{output}"
    );
    assert!(!output.contains("File not found"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_original_vmstat_top_terminal_output_read_misroute_is_targeted() {
    if !cfg!(target_os = "macos")
        || Command::new("vm_stat").output().is_err()
        || Command::new("top")
            .args(["-l", "1", "-o", "cpu"])
            .output()
            .is_err()
    {
        return;
    }

    let home = temp_shell_home("cosh-core-shell-evidence-vmstat-top");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-vmstat-top","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *evidence-original-vmstat-top*)
    printf '%s\n' '{"type":"control_request","request_id":"original-list-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-original-list","action":"list_commands","limit":5}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command: vm_stat'*'command: top -l 1 -o cpu | head -30'*'output_id: terminal-output://raw-session-'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-vmstat-top","is_error":true,"result":"missing vm_stat/top command index"}'; exit 1 ;;
    esac
    output_tail=${response1#*output_id: }
    output_id=${output_tail%%\\n*}
    case "$output_id" in
      terminal-output://raw-session-*/cmd-*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-vmstat-top","is_error":true,"result":"invalid original output id"}'; exit 1 ;;
    esac
    printf '%s\n' "{\"type\":\"assistant\",\"session_id\":\"sess-cosh-core-shell-evidence-vmstat-top\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu-original-misroute\",\"name\":\"read_file\",\"input\":{\"path\":\"$output_id\"}}]}}"
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-vmstat-top","message":{"content":[{"type":"text","text":"ORIGINAL VMSTAT TOP MISROUTE AUDITED"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-vmstat-top","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-vmstat-top","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"vm_stat\n".to_vec(), Duration::ZERO),
            (
                b"top -l 1 -o cpu | head -30\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"?? \xe5\x88\x86\xe6\x9e\x90\xe4\xb8\x80\xe4\xb8\x8b\xe6\x9c\x80\xe8\xbf\x91\xe4\xb8\xa4\xe6\xac\xa1\xe5\x91\xbd\xe4\xbb\xa4\xe7\x9a\x84\xe8\xbe\x93\xe5\x87\xba\xe7\xbb\x93\xe6\x9e\x9c evidence-original-vmstat-top\n".to_vec(),
                Duration::from_millis(2_000),
            ),
            (
                b"/details evidence-1\n/details tool-1\n".to_vec(),
                Duration::from_millis(6_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("ORIGINAL VMSTAT TOP MISROUTE AUDITED"),
        "{output}"
    );
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("Activity details tool-1"), "{output}");
    assert!(output.contains("vm_stat"), "{output}");
    assert!(output.contains("top -l 1 -o cpu | head -30"), "{output}");
    assert!(
        output.contains("virtual_evidence_read_misroute: true"),
        "{output}"
    );
    assert!(
        output.contains("misrouted_output_id: terminal-output://raw-session-"),
        "{output}"
    );
    assert!(
        output.contains("recommended_action: cosh_shell_evidence_read_output"),
        "{output}"
    );
    assert!(!output.contains("File not found"), "{output}");
    assert!(
        !output.contains("missing vm_stat/top command index"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_failed_command_diagnostic_reads_output_before_answering() {
    let home = temp_shell_home("cosh-core-shell-evidence-failed-diagnostic");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-failed-diagnostic","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *evidence-failed-diagnostic*)
    printf '%s\n' '{"type":"control_request","request_id":"failed-list-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-failed-list","action":"list_commands","limit":5}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'status: failed'*'output_id: terminal-output://raw-session-'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-failed-diagnostic","is_error":true,"result":"missing failed command index"}'; exit 1 ;;
    esac
    output_tail=${response1#*output_id: }
    output_id=${output_tail%%\\n*}
    printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"failed-read-1\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-failed-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":5}}"
    IFS= read -r response2 || exit 2
    case "$response2" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'action: read_output'*'No such file or directory'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-failed-diagnostic","is_error":true,"result":"missing failed command output excerpt"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-failed-diagnostic","message":{"content":[{"type":"text","text":"FAILED DIAGNOSTIC READ OUTPUT FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-failed-diagnostic","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-failed-diagnostic","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"ls /path/that/does/not/exist\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? \xe4\xb8\xba\xe4\xbb\x80\xe4\xb9\x88\xe5\xa4\xb1\xe8\xb4\xa5 evidence-failed-diagnostic\n".to_vec(),
                Duration::from_millis(1_200),
            ),
            (
                b"/details evidence-1\n/details evidence-2\n".to_vec(),
                Duration::from_millis(6_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("No such file or directory"), "{output}");
    assert!(
        output.contains("FAILED DIAGNOSTIC READ OUTPUT FINAL"),
        "{output}"
    );
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("Activity details evidence-2"), "{output}");
    assert!(output.contains("action: list_commands"), "{output}");
    assert!(output.contains("action: read_output"), "{output}");
    assert!(
        output.contains("output_id: terminal-output://raw-session-"),
        "{output}"
    );
    assert!(
        !output.contains("missing diagnostic evidence prompt policy"),
        "{output}"
    );
    assert!(
        !output.contains("missing failed command output excerpt"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_list_recent_commands_does_not_read_output() {
    let home = temp_shell_home("cosh-core-shell-evidence-list-only");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-list-only","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *evidence-list-only*)
    printf '%s\n' '{"type":"control_request","request_id":"list-only-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-list-only","action":"list_commands","limit":5}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'output_id: terminal-output://raw-session-'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-list-only","is_error":true,"result":"missing list-only command index"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-list-only","message":{"content":[{"type":"text","text":"LIST ONLY FACTS FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-list-only","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-list-only","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"printf 'list-only-output\\n'\n".to_vec(), Duration::ZERO),
            (
                b"?? \xe5\x88\x97\xe4\xb8\x80\xe4\xb8\x8b\xe6\x9c\x80\xe8\xbf\x91\xe5\x91\xbd\xe4\xbb\xa4 evidence-list-only\n".to_vec(),
                Duration::from_secs(1),
            ),
            (b"/details evidence-1\n".to_vec(), Duration::from_millis(3_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("list-only-output"), "{output}");
    assert!(output.contains("LIST ONLY FACTS FINAL"), "{output}");
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("action: list_commands"), "{output}");
    assert!(!output.contains("action: read_output"), "{output}");
    assert!(!output.contains("Activity details evidence-2"), "{output}");
    assert!(
        !output.contains("missing list-only evidence prompt policy"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_activity_recap_lists_facts_without_reading_output() {
    let home = temp_shell_home("cosh-core-shell-evidence-activity-recap");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-activity-recap","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *evidence-activity-recap*)
    printf '%s\n' '{"type":"control_request","request_id":"activity-recap-list-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-activity-recap-list","action":"list_commands","limit":5}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'command_id: cmd-1'*'output_id: terminal-output://raw-session-'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-activity-recap","is_error":true,"result":"missing activity recap command index"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-activity-recap","message":{"content":[{"type":"text","text":"ACTIVITY RECAP FACTS ONLY FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-activity-recap","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-activity-recap","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"printf 'activity-recap-output\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? \xe6\x9c\x80\xe8\xbf\x91\xe5\x9c\xa8\xe5\xb9\xb2\xe5\x98\x9b evidence-activity-recap\n".to_vec(),
                Duration::from_secs(1),
            ),
            (b"/details evidence-1\n".to_vec(), Duration::from_millis(5_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("activity-recap-output"), "{output}");
    assert!(
        output.contains("ACTIVITY RECAP FACTS ONLY FINAL"),
        "{output}"
    );
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("action: list_commands"), "{output}");
    assert!(!output.contains("action: read_output"), "{output}");
    assert!(!output.contains("Activity details evidence-2"), "{output}");
    assert!(
        !output.contains("missing activity recap evidence prompt policy"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_status_analysis_reads_result_bearing_output() {
    let home = temp_shell_home("cosh-core-shell-evidence-status-analysis");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-status-analysis","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *evidence-status-analysis*)
    printf '%s\n' '{"type":"control_request","request_id":"status-list-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-status-list","action":"list_commands","limit":5}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceCommandIndex'*'status: failed'*'output_id: terminal-output://raw-session-'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-status-analysis","is_error":true,"result":"missing status command index"}'; exit 1 ;;
    esac
    output_tail=${response1#*output_id: }
    output_id=${output_tail%%\\n*}
    printf '%s\n' "{\"type\":\"control_request\",\"request_id\":\"status-read-1\",\"request\":{\"subtype\":\"shell_evidence\",\"tool_use_id\":\"toolu-status-read\",\"action\":\"read_output\",\"output_id\":\"$output_id\",\"direction\":\"tail\",\"lines\":5}}"
    IFS= read -r response2 || exit 2
    case "$response2" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'No such file or directory'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-status-analysis","is_error":true,"result":"missing status output excerpt"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-status-analysis","message":{"content":[{"type":"text","text":"STATUS ANALYSIS READ OUTPUT FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-status-analysis","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-status-analysis","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (
                b"ls /path/that/does/not/exist\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? \xe5\x88\x86\xe6\x9e\x90\xe6\x9c\x80\xe8\xbf\x91\xe7\x8a\xb6\xe6\x80\x81 evidence-status-analysis\n".to_vec(),
                Duration::from_secs(1),
            ),
            (
                b"/details evidence-1\n/details evidence-2\n".to_vec(),
                Duration::from_millis(3_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("STATUS ANALYSIS READ OUTPUT FINAL"),
        "{output}"
    );
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("Activity details evidence-2"), "{output}");
    assert!(output.contains("action: list_commands"), "{output}");
    assert!(output.contains("action: read_output"), "{output}");
    assert!(
        !output.contains("missing status output excerpt"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_stale_terminal_output_ref_fails_closed() {
    let home = temp_shell_home("cosh-core-shell-evidence-stale-ref");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-stale-ref","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-stale-terminal-output*)
    printf '%s\n' '{"type":"control_request","request_id":"stale-read-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu-stale-read","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","direction":"tail","lines":5}}'
    IFS= read -r response1 || exit 2
    case "$response1" in
      *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'reason: stale_session'*) ;;
      *'"behavior":"shell_evidence"'*'ShellEvidenceExcerpt'*'reason: not_in_current_ledger'*) ;;
      *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-stale-ref","is_error":true,"result":"missing stale shell evidence rejection"}'; exit 1 ;;
    esac
    case "$response1" in
      *new-session-output*) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-stale-ref","is_error":true,"result":"stale evidence leaked current cmd-1 output"}'; exit 1 ;;
    esac
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-stale-ref","message":{"content":[{"type":"text","text":"STALE EVIDENCE REJECTED"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-stale-ref","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-stale-ref","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"printf 'new-session-output\\n'\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-stale-terminal-output\n".to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"/details evidence-1\n".to_vec(),
                Duration::from_millis(5_000),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("new-session-output"), "{output}");
    assert!(output.contains("STALE EVIDENCE REJECTED"), "{output}");
    assert!(output.contains("Activity details evidence-1"), "{output}");
    assert!(output.contains("action: read_output"), "{output}");
    assert!(
        output.contains("output_id: terminal-output://raw-session/cmd-1"),
        "{output}"
    );
    assert!(output.contains("Shell evidence failed"), "{output}");
    assert!(
        output.contains("Headline: shell evidence unavailable"),
        "{output}"
    );
    assert!(output.contains("Status: failed"), "{output}");
    assert!(
        !output.contains("stale evidence leaked current cmd-1 output"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_recommend_mode_suppresses_shell_evidence_instructions() {
    let home = temp_shell_home("cosh-core-shell-evidence-recommend");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-shell-evidence-recommend","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh_shell_evidence*|*'```cosh-request'*) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-recommend","is_error":true,"result":"recommend prompt exposed shell evidence request instructions"}'; exit 1 ;;
esac
case "$user_message" in
  *'say when shell evidence is needed instead of requesting it automatically'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-shell-evidence-recommend","is_error":true,"result":"recommend prompt missing evidence limitation"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-shell-evidence-recommend","message":{"content":[{"type":"text","text":"RECOMMEND EVIDENCE INSTRUCTIONS SUPPRESSED"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-shell-evidence-recommend","is_error":false,"result":"done"}'
exit 0
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval recommend\n".to_vec(), Duration::ZERO),
            (
                b"printf 'recommend-output\\n'\n".to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"?? cosh-core-recommend-evidence-check\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Mode set to recommend."), "{output}");
    assert!(
        output.contains("RECOMMEND EVIDENCE INSTRUCTIONS SUPPRESSED"),
        "{output}"
    );
    assert!(
        !output.contains("recommend prompt exposed shell evidence request instructions"),
        "{output}"
    );
    assert!(!output.contains("Agent Requested Evidence"), "{output}");
}
