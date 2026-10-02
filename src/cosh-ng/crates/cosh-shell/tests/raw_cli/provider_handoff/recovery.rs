use super::*;

#[test]
fn raw_cli_shell_handoff_resume_timeout_retries_without_timeout_card() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider resume timeout shell trigger resume timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(6_000)),
        ],
    );

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ ssh -V"), "{output}");
    assert!(
        output.contains("Command result analysis for req-1: foreground shell evidence received"),
        "{output}"
    );
    assert!(
        output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
    assert!(
        output.contains("Provider session continuity may be degraded."),
        "{output}"
    );
    assert!(
        output.contains("Recovery trigger: provider_timeout"),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(
        !output.contains("No provider response within 20s"),
        "{output}"
    );
}

// The first fallback turn (T2) retries within the same provider
// session, so the "Agent recovery" fresh-turn panel (whose copy claims a
// fresh provider turn and degraded continuity) must not be shown for it;
// T2 renders the trigger-reason and retry status lines instead. The fake
// adapter keeps timing out until resume is disabled, so the chain escalates
// to the final fresh safety net (T3), which renders the panel exactly once,
// after the T2 lines.
#[test]
fn raw_cli_shell_handoff_fallback_chain_renders_retry_line_then_single_panel() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider resume timeout shell trigger resume timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(6_000)),
        ],
    );

    // T2 renders the trigger reason plus the same-session retry line before
    // any panel (a successful T2 never reaches the T3 panel, so the reason
    // must already be visible here).
    assert_ordered(
        &output,
        &[
            "Recovery trigger: provider_timeout",
            "Provider turn stalled; retrying with session history...",
            "Using a fresh provider turn for shell evidence recovery.",
        ],
    );
    // The fresh-turn panel appears exactly once (T3 only, never for T2).
    assert_eq!(
        count_occurrences(
            &output,
            "Using a fresh provider turn for shell evidence recovery."
        ),
        1,
        "{output}"
    );
    assert_eq!(
        count_occurrences(&output, "Provider session continuity may be degraded."),
        1,
        "{output}"
    );
}

// When the T2 same-session retry succeeds,
// the chain never reaches the T3 panel, so the recovery trigger reason must
// already be part of the T2 notice; the fresh-turn panel must not render.
#[test]
fn raw_cli_shell_handoff_retry_success_shows_reason_without_panel() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider resume timeout shell trigger resume timeout once\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(6_000)),
        ],
    );

    // T1 times out once; T2 succeeds with the reason line + retry line.
    assert_ordered(
        &output,
        &[
            "Recovery trigger: provider_timeout",
            "Provider turn stalled; retrying with session history...",
            "Command result analysis for req-1: foreground shell evidence received",
        ],
    );
    // The T3 fresh-turn panel copy must never appear on the success path.
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
    assert!(
        !output.contains("Provider session continuity may be degraded."),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

#[test]
fn raw_cli_shell_handoff_resume_timeout_renders_structured_context_before_recovery_notice() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider resume timeout shell structured before recovery\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(6_000)),
        ],
    );

    assert!(
        output.contains("Approved req-1") || output.contains("Auto-approved req-1"),
        "{output}"
    );
    assert!(
        output.contains("$ printf structured-before-recovery"),
        "{output}"
    );
    assert_ordered(
        &output,
        &[
            "Using a fresh provider turn for shell evidence recovery.",
            "Command result analysis for req-1: foreground shell evidence received",
        ],
    );
    assert!(
        !output.contains("Skill failed: recovery-context"),
        "{output}"
    );
    assert_eq!(
        count_occurrences(
            &output,
            "Using a fresh provider turn for shell evidence recovery."
        ),
        1,
        "{output}"
    );
    // The trigger reason renders once in the T2 same-session retry notice
    // and once inside the T3 fresh-turn panel (a successful T2 must already
    // show the reason, so the full chain shows it twice).
    assert_eq!(
        count_occurrences(&output, "Recovery trigger: provider_timeout"),
        2,
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(
        !output.contains("No provider response within 20s"),
        "{output}"
    );
}

#[test]
fn raw_cli_shell_handoff_recovery_uses_zh_language_env() {
    let home = temp_shell_home("handoff-recovery-zh");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("ssh"),
        "#!/bin/sh\nprintf 'OpenSSH_fake_for_recovery\\n'\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "zh-CN"),
            ("HOME", &home_str),
            ("PATH", &path),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider resume timeout shell trigger resume timeout\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(6_000)),
        ],
    );

    assert!(output.contains("已批准 req-1"), "{output}");
    assert!(output.contains("$ ssh -V"), "{output}");
    assert!(output.contains("OpenSSH_fake_for_recovery"), "{output}");
    assert!(output.contains("Agent 恢复"), "{output}");
    assert!(
        output.contains("正在使用新的 provider 轮次恢复 shell evidence。"),
        "{output}"
    );
    assert!(output.contains("Provider 会话连续性可能降低。"), "{output}");
    assert!(
        output.contains("恢复触发原因：provider_timeout"),
        "{output}"
    );
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
    assert!(
        !output.contains("Provider session continuity may be degraded."),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
    assert!(
        !output.contains("No provider response within 20s"),
        "{output}"
    );
}

#[test]
fn raw_cli_zh_provider_timeout_drops_extra_queued_requests() {
    let home = temp_shell_home("qwen-timeout-dropped-queue-zh");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *first-timeout*)
      sleep 30
      exit 0
      ;;
    *queued-one*)
      printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-timeout-queue","model":"qwen-test"}'
      printf '%s\n' '{"type":"assistant","session_id":"sess-timeout-queue","message":{"content":[{"type":"text","text":"Queued request one completed."}]}}'
      printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-timeout-queue","is_error":false,"result":"done"}'
      exit 0
      ;;
    *queued-two*)
      printf '%s\n' '{"type":"assistant","session_id":"sess-timeout-queue","message":{"content":[{"type":"text","text":"Queued request two should have been dropped."}]}}'
      printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-timeout-queue","is_error":false,"result":"done"}'
      exit 0
      ;;
  esac
done
sleep 30
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
            ("COSH_SHELL_LANG", "zh-CN"),
            ("COSH_AGENT_START_TIMEOUT_SECS", "1"),
        ],
        vec![
            (b"?? first-timeout\n".to_vec(), Duration::ZERO),
            (b"?? queued-one\n".to_vec(), Duration::from_millis(100)),
            (b"?? queued-two\n".to_vec(), Duration::from_millis(100)),
            (b"exit 0\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("provider 超时后已跳过 1 个排队请求"),
        "{output}"
    );
    assert!(output.contains("Queued request one completed."), "{output}");
    assert!(
        !output.contains("Queued request two should have been dropped."),
        "{output}"
    );
    assert!(
        !output.contains("1 queued requests skipped after provider timeout"),
        "{output}"
    );
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
}
