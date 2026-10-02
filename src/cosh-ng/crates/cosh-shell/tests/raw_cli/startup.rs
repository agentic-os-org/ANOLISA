use super::*;
use ratatui::text::Span;

#[test]
fn help_flag_writes_to_stdout_not_stderr() {
    // POSIX/GNU convention: --help output goes to stdout, not stderr.
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = Command::new(binary)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cosh-shell --help");

    assert!(output.status.success(), "--help should exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("Usage: cosh-shell"),
        "--help text should be on stdout, got stdout={:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "--help should not write to stderr, got stderr={:?}",
        stderr
    );
}

#[test]
fn version_flag_writes_to_stdout_not_stderr() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cosh-shell --version");

    assert!(output.status.success(), "--version should exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("cosh-shell"),
        "--version text should be on stdout, got stdout={:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "--version should not write to stderr, got stderr={:?}",
        stderr
    );
}

#[test]
fn diagnostics_export_help_writes_to_stdout_not_stderr() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = Command::new(binary)
        .args(["diagnostics", "export", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cosh-shell diagnostics export --help");

    assert!(
        output.status.success(),
        "diagnostics export --help should exit 0"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("Usage:"),
        "diagnostics export --help should write to stdout, got stdout={:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "diagnostics export --help should not write to stderr, got stderr={:?}",
        stderr
    );
}

#[test]
fn raw_cli_startup_banner_renders_when_enabled() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Adapter: fake"), "{output}");
    assert!(output.contains("Shell: bash"), "{output}");
    assert!(output.contains("Approval: auto"), "{output}");
    assert!(output.contains("Analysis: smart"), "{output}");
    assert!(!output.contains("Mode: auto"), "{output}");
    assert!(output.contains("/help"), "{output}");
    assert!(output.contains("/hooks"), "{output}");
    assert!(!output.contains("/explain"), "{output}");
    assert!(
        !output.contains("┌─┐┌─┐┌─┐┬ ┬"),
        "logo should be removed: {output}"
    );
    assert!(
        !output.contains("Agent actions still require approval"),
        "footer should be removed: {output}"
    );
    assert!(
        !output.contains("Startup hooks: none configured"),
        "no hooks line when hooks are disabled: {output}"
    );
    assert!(!output.contains("no command ran"), "{output}");
    assert!(!output.contains("cosh-osc$ ╭ · cosh-shell"), "{output}");
    assert_inline_before_followup(&output, "╭ · cosh-shell", "exit");
    assert!(!output.contains("Thinking..."), "{output}");
}

#[test]
fn raw_cli_startup_banner_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "zh-CN"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("后端: fake"), "{output}");
    assert!(output.contains("Shell: bash"), "{output}");
    assert!(output.contains("审批: auto"), "{output}");
    assert!(output.contains("分析: smart"), "{output}");
    assert!(!output.contains("模式: auto"), "{output}");
    assert!(output.contains("/help"), "{output}");
}

#[test]
fn raw_cli_startup_banner_reports_effective_modes() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_APPROVAL_MODE", "trust"),
            ("COSH_SHELL_ANALYSIS_MODE", "manual"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("Approval: trust"), "{output}");
    assert!(output.contains("Analysis: manual"), "{output}");
}

#[test]
fn raw_cli_startup_approval_alias_and_invalid_value_fail_closed() {
    for value in ["balanced", "invalid"] {
        let output = run_raw_cli_with_env(
            "fake",
            "exit\n",
            &[
                ("COSH_SHELL_STARTUP_BANNER", "1"),
                ("COSH_SHELL_APPROVAL_MODE", value),
                ("TERM", "xterm-256color"),
            ],
        );

        assert!(output.contains("Approval: recommend"), "{value}: {output}");
        assert!(!output.contains("Approval: auto"), "{value}: {output}");
    }
}

#[test]
fn raw_cli_plain_startup_banner_keeps_both_modes() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_RENDER", "plain"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("Approval: auto"), "{output}");
    assert!(output.contains("Analysis: smart"), "{output}");
}

#[test]
fn raw_cli_startup_health_fixture_renders_when_enabled() {
    let cwd = temp_shell_home("startup-health-fixture");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains("Load  1m"), "{output}");
    assert!(!output.contains("Load  Load 1m"), "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(!output.contains("Mem avail 94%"), "{output}");
    assert!(output.contains("Swap used"), "{output}");
    assert!(output.contains("Findings"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(output.contains("[Health]"), "{output}");
    assert_eq!(output.matches("[Health]").count(), 3, "{output}");
    assert!(!output.contains("Next:"), "{output}");
    assert_inline_before_followup(&output, "╭─ Health check", "exit");
}

#[test]
fn raw_cli_startup_health_disabled_renders_no_row() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "disabled"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("/help"), "{output}");
    assert!(!output.contains("Health: ok"), "{output}");
    assert!(!output.contains("Health check"), "{output}");
}

#[test]
fn raw_cli_startup_health_cards_share_configured_width() {
    let cwd = temp_shell_home("startup-health-card-width");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_WIDTH", "140"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![
            (b"?? hello\n".to_vec(), Duration::from_millis(1600)),
            (b"exit\n".to_vec(), Duration::from_millis(900)),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("Received shell prompt request"), "{output}");
    let widths = box_line_widths(&output);
    assert!(
        widths.len() >= 3,
        "expected startup, health and agent boxes\n{output}"
    );
    assert!(
        widths.iter().all(|width| *width == 140),
        "box widths should match configured width: {widths:?}\n{output}"
    );
}

#[test]
fn raw_cli_startup_health_critical_fixture_uses_compact_oom_copy() {
    let cwd = temp_shell_home("startup-health-critical-fixture");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-critical"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("critical"), "{output}");
    assert!(output.contains("OOM"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(
        output.contains("cause of the most recent OOM")
            || output.contains("why the latest OOM killed"),
        "{output}"
    );
    assert!(!output.contains("CONSTRAINT_"), "{output}");
    assert!(!output.contains("current pressure"), "{output}");
    assert!(!output.contains("OOM age"), "{output}");
    assert!(!output.contains("process python"), "{output}");
    assert!(!output.contains("pid "), "{output}");
    assert!(!output.contains("constraint "), "{output}");
    assert!(!output.contains("Next:"), "{output}");
    assert!(!output.contains("██████████"), "{output}");
}

#[test]
fn raw_cli_startup_health_degraded_fixture_is_read_only() {
    let cwd = temp_shell_home("startup-health-degraded-read-only");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-degraded"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![
            (b"\t\n".to_vec(), Duration::from_millis(1400)),
            (b"exit\n".to_vec(), Duration::from_millis(700)),
        ],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("degraded"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(output.contains("[Health]"), "{output}");
    assert!(!output.contains("[Personal]"), "{output}");
    assert!(!output.contains("Tab insert"), "{output}");
    assert!(!output.contains("Enter ask"), "{output}");
    assert!(!output.contains("Shift+Tab cycle"), "{output}");
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_healthy_fixture_renders_startup_row_in_banner() {
    let cwd = temp_shell_home("startup-health-healthy-fixture");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-healthy"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::from_millis(150))],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Health: ok"), "{output}");
    // The banner path marks the report rendered, so the deferred path must
    // not emit the row a second time.
    assert_eq!(output.matches("Health: ok").count(), 1, "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(output.contains("Disk"), "{output}");
    assert!(!output.contains("Health check"), "{output}");
    assert!(!output.contains("Suggested prompts"), "{output}");
    assert_inline_before_followup(&output, "╭ · cosh-shell", "exit");
}

#[test]
fn raw_cli_startup_health_no_color_keeps_readable_content() {
    let cwd = temp_shell_home("startup-health-no-color");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("NO_COLOR", "1"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains("Load  1m"), "{output}");
    assert!(!output.contains("Load  Load 1m"), "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(!output.contains("Mem avail 94%"), "{output}");
    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(output.contains("[Health]"), "{output}");
    assert!(!output.contains("Next:"), "{output}");
    let health_block = output
        .split("Health check")
        .nth(1)
        .unwrap_or(output.as_str())
        .split("Suggested prompts")
        .next()
        .unwrap_or(output.as_str());
    assert!(!health_block.contains("\x1b["), "{output}");
}

#[test]
fn raw_cli_startup_health_dumb_terminal_uses_plain_fallback() {
    let cwd = temp_shell_home("startup-health-dumb");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "dumb"),
        ],
        &cwd,
        &[("Mem used", b"exit\n")],
    );

    assert!(output.contains("Health check:"), "{output}");
    assert!(output.contains("warning"), "{output}");
    assert!(output.contains("Load  1m"), "{output}");
    assert!(!output.contains("Load  Load 1m"), "{output}");
    assert!(output.contains("Mem used"), "{output}");
    assert!(!output.contains("Mem avail 94%"), "{output}");
    assert!(!output.contains("Suggested prompts"), "{output}");
    assert!(!output.contains("Shift+Tab cycle"), "{output}");
    assert!(!output.contains("▕"), "{output}");
    assert!(!output.contains("Next:"), "{output}");
    assert!(!output.contains('╭'), "{output}");
    assert!(!output.contains('│'), "{output}");
    assert!(!output.contains('╰'), "{output}");
}

#[test]
fn raw_cli_startup_health_suppresses_all_three_visible_try_items() {
    let cwd = temp_shell_home("startup-health-suppressed-try");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let env = [
        ("COSH_SHELL_STARTUP_BANNER", "1"),
        ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
        (
            "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
            suppression_store.as_str(),
        ),
        ("COSH_SHELL_LANG", "en-US"),
        ("TERM", "xterm-256color"),
        ("COSH_SHELL_ISOLATED", "0"),
    ];

    let first = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &env,
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );
    assert!(first.contains("Suggested prompts"), "{first}");
    assert!(!first.contains("Next:"), "{first}");

    let second = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &env,
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::from_millis(150))],
    );
    assert!(second.contains("Health check"), "{second}");
    assert!(second.contains("warning"), "{second}");
    assert!(!second.contains("Suggested prompts"), "{second}");
    assert!(first_health_prompt(&second).is_none(), "{second}");
    assert!(!second.contains("Next:"), "{second}");
}

#[test]
fn raw_cli_startup_health_banner_disabled_does_not_suppress_later_prompt() {
    let cwd = temp_shell_home("startup-health-hidden-no-suppress");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store_str = suppression_store.to_string_lossy().into_owned();

    let hidden = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store_str.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::from_millis(150))],
    );
    assert!(!hidden.contains("Health check"), "{hidden}");
    assert!(
        !suppression_store.exists(),
        "hidden health scan should not persist suppression"
    );

    let shown = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store_str.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[("Suggested prompts", b"exit\n")],
    );

    assert!(shown.contains("Health check"), "{shown}");
    assert!(shown.contains("Suggested prompts"), "{shown}");
}

#[test]
fn raw_cli_startup_health_prompt_ghost_tab_fills_first_suggestion() {
    let cwd = temp_shell_home("startup-health-ghost-tab");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_RENDER", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[
            (
                " › Analyze memory pressure and identify top consumers",
                b"\t\n",
            ),
            ("Received shell prompt request", b"exit\n"),
        ],
    );

    assert!(output.contains("Suggested prompts"), "{output}");
    assert!(
        output.contains("Analyze memory pressure and identify top consumers"),
        "{output}"
    );
    let first_prompt = first_health_prompt(&output).expect("first health prompt");
    let compact = compact_without_box_chars(&output);
    assert!(
        compact.contains(&format!("Received shell prompt request: {first_prompt}")),
        "{output}"
    );
    assert!(
        compact.contains("Runtime context hints visible to Agent"),
        "{output}"
    );
    assert!(compact.contains("health_scan"), "{output}");
    assert!(compact.contains("scan_id=health-"), "{output}");
    assert!(compact.contains("bounded_facts_only=true"), "{output}");
    assert!(
        !output.contains("command not found: Analyze memory pressure"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_prompt_ghost_tab_only_does_not_submit() {
    let cwd = temp_shell_home("startup-health-ghost-tab-only");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_RENDER", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![
            (b"\t".to_vec(), Duration::from_millis(1400)),
            (vec![0x15], Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(700)),
        ],
    );

    assert!(first_health_prompt(&output).is_some(), "{output}");
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_shift_tab_cycles_then_enter_submits_active_prompt() {
    let cwd = temp_shell_home("startup-health-shift-tab-enter");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        &[
            (
                " › Analyze memory pressure and identify top consumers",
                b"\x1b[Z",
            ),
            (
                " › Check whether swap pressure is active and which processes",
                b"\n",
            ),
            (
                "Received shell prompt request: Check whether swap pressure is active",
                b"exit\n",
            ),
        ],
    );

    assert_eq!(output.matches("[Health]").count(), 3, "{output}");
    assert!(
        output.contains("\x1b[2m › Check whether swap pressure"),
        "{output}"
    );
    assert!(
        compact_without_box_chars(&output).contains(
            "Received shell prompt request: Check whether swap pressure is active and which processes"
        ),
        "{output}"
    );
    assert!(!output.contains("command not found"), "{output}");
}

#[test]
fn raw_cli_startup_health_prompt_selection_respects_terminal_capability_not_color() {
    for (name, extra_env) in [
        ("dumb", vec![("TERM", "dumb")]),
        (
            "plain",
            vec![("TERM", "xterm-256color"), ("COSH_SHELL_RENDER", "plain")],
        ),
        (
            "no-color",
            vec![("TERM", "xterm-256color"), ("NO_COLOR", "1")],
        ),
    ] {
        let cwd = temp_shell_home(&format!("startup-health-ghost-disabled-{name}"));
        let suppression_store = cwd.join("health-suppression");
        let suppression_store = suppression_store.to_string_lossy().into_owned();
        let mut env = vec![
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ISOLATED", "0"),
        ];
        env.extend(extra_env);
        let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
            "fake",
            &[],
            &env,
            &cwd,
            vec![
                (b"\t\n".to_vec(), Duration::from_millis(1400)),
                (b"exit\n".to_vec(), Duration::from_millis(700)),
            ],
        );

        if name == "dumb" {
            assert!(!output.contains("Suggested prompts"), "{name}\n{output}");
            assert!(first_health_prompt(&output).is_none(), "{name}\n{output}");
            assert!(
                !output.contains("Received shell prompt request:"),
                "{name}\n{output}"
            );
        } else {
            assert!(output.contains("Suggested prompts"), "{name}\n{output}");
            assert!(first_health_prompt(&output).is_some(), "{name}\n{output}");
            assert!(
                output.contains("Received shell prompt request:"),
                "{name}\n{output}"
            );
        }
    }
}

#[test]
fn raw_cli_startup_health_prompt_ghost_does_not_override_manual_input() {
    let cwd = temp_shell_home("startup-health-ghost-manual");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_RENDER", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_ISOLATED", "0"),
        ],
        &cwd,
        vec![
            (b"echo manual\n".to_vec(), Duration::from_millis(1400)),
            (b"exit\n".to_vec(), Duration::from_millis(700)),
        ],
    );

    assert!(first_health_prompt(&output).is_some(), "{output}");
    assert!(output.contains("echo manual"), "{output}");
    assert!(output.contains("manual"), "{output}");
    assert!(
        !output.contains("Received shell prompt request: Analyze memory pressure"),
        "{output}"
    );
    assert!(
        !output.contains("command not found: Analyze memory pressure"),
        "{output}"
    );
}

#[test]
fn raw_cli_startup_health_context_is_not_attached_to_plain_agent_request() {
    let cwd = temp_shell_home("startup-health-context");
    let suppression_store = cwd.join("health-suppression");
    let suppression_store = suppression_store.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning"),
            (
                "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
                suppression_store.as_str(),
            ),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![
            (
                b"please show context\n".to_vec(),
                Duration::from_millis(1_000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Health check"), "{output}");
    assert!(
        output.contains("Runtime context hints visible to Agent"),
        "{output}"
    );
    let compact = compact_terminal_words(&output);
    assert!(compact.contains("Runtime context hints visible to Agent: <none>"));
    assert!(!compact.contains("health_scan"), "{output}");
    assert!(!compact.contains("scan_id=health-"), "{output}");
    assert!(!compact.contains("bounded_facts_only=true"), "{output}");
    assert!(!output.contains("journalctl -k"), "{output}");
    let context_tail = output
        .split("Runtime context hints visible to Agent")
        .nth(1)
        .unwrap_or("");
    let context_card = context_tail.split('╰').next().unwrap_or(context_tail);
    assert!(!context_card.contains("/tmp/cosh"), "{output}");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn raw_cli_startup_health_live_does_not_render_on_non_linux_by_default() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(!output.contains("Health check"), "{output}");
    assert!(!output.contains("Health:"), "{output}");
    assert!(!output.contains("platform unsupported"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_startup_health_live_does_not_block_shell_on_linux_by_default() {
    let output = run_raw_cli_with_env(
        "fake",
        "echo after-health-startup\nexit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("after-health-startup"), "{output}");
    assert!(!output.contains("platform unsupported"), "{output}");
}

#[test]
fn raw_cli_startup_hooks_use_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_STARTUP_HOOKS", "1"),
            ("COSH_SHELL_LANG", "zh-CN"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(
        output.contains("启动 hooks: 内置只读检查已完成"),
        "{output}"
    );
    assert!(output.contains("启动检查结果"), "{output}");
    assert!(
        output.contains("检测到 Cargo.toml Rust 项目")
            || output.contains("内置只读检查未发现启动项"),
        "{output}"
    );
    assert!(
        output.contains("cosh-shell 只检查了轻量启动上下文"),
        "{output}"
    );
    for label in [
        "Startup hooks:",
        "Startup findings",
        "Rust project detected",
        "No startup findings from built-in read-only checks",
        "only inspected lightweight startup context",
    ] {
        assert!(
            !output.contains(label),
            "startup English UI label leaked: {label}\n{output}"
        );
    }
}

#[test]
fn raw_cli_startup_hooks_no_findings_use_zh_language_env() {
    let cwd = temp_shell_home("startup-hooks-no-findings");
    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_STARTUP_HOOKS", "1"),
            ("COSH_SHELL_LANG", "zh-CN"),
            ("TERM", "xterm-256color"),
        ],
        &cwd,
        vec![(b"exit\n".to_vec(), Duration::ZERO)],
    );

    assert!(
        output.contains("启动 hooks: 内置只读检查已完成"),
        "{output}"
    );
    assert!(output.contains("启动检查结果"), "{output}");
    assert!(output.contains("内置只读检查未发现启动项"), "{output}");
    assert!(
        !output.contains("No startup findings from built-in read-only checks"),
        "{output}"
    );
}

#[test]
fn raw_cli_default_agent_mode_defers_safe_fallback_tool() {
    let home = temp_shell_home("default-agent-auto");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? request tool approval\n"),
            ("Deferred req-1", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Approval: auto"), "{output}");
    assert!(output.contains("Analysis: smart"), "{output}");
    assert!(output.contains("Deferred req-1"), "{output}");
    assert!(output.contains("$ git status"), "{output}");
    assert!(!output.contains("Approval req-"), "{output}");
    assert!(!output.contains("[ Allow once ]"), "{output}");
    assert!(!output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Auto-approved req-1"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        !output.contains("evidence: ShellCommandCompleted"),
        "{output}"
    );
}

#[test]
fn raw_cli_raw_run_without_adapter_uses_cosh_core_default_adapter() {
    let home = temp_shell_home("cosh-core-default-adapter");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
case "$init" in
  *'"subtype":"initialize"'*) ;;
  *) printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-default","is_error":true,"result":"missing initialize"}'; exit 1 ;;
esac
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-default","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-default-adapter-smoke*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-default","message":{"content":[{"type":"text","text":"Cosh-core default adapter reached via implicit raw."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-default","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-default","is_error":true,"result":"unexpected prompt"}'
"#,
    );

    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_default_with_args_env_and_delayed_input(
        &["--run"],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
        ],
        vec![
            (
                b"?? cosh-core-default-adapter-smoke\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/debug session\n".to_vec(), Duration::from_millis(500)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Adapter: cosh-core"), "{output}");
    assert!(
        output.contains("Cosh-core default adapter reached via implicit raw."),
        "{output}"
    );
    assert!(output.contains("provider invocation:"), "{output}");
    assert!(
        output.contains("cosh-raw-cli-cosh-core-default-adapter"),
        "{output}"
    );
    assert!(output.contains("/bin/cosh-core"), "{output}");
    assert!(!output.contains("Adapter: fake"), "{output}");
    assert!(!output.contains("unexpected prompt"), "{output}");
    assert!(!output.contains("failed to run cosh-core"), "{output}");
}

#[test]
fn raw_cli_startup_hooks_render_markdown_findings_without_running_commands() {
    let output = run_raw_cli_with_env(
        "fake",
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_STARTUP_HOOKS", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(
        output.contains("Startup hooks: built-in read-only checks completed"),
        "{output}"
    );
    assert!(output.contains("Startup findings"), "{output}");
    assert!(
        output.contains("Rust project detected from Cargo.toml")
            || output.contains("No startup findings from built-in read-only checks"),
        "{output}"
    );
    assert!(
        output.contains("cosh-shell only inspected lightweight startup context"),
        "{output}"
    );
    assert!(
        !output.contains("Read-only startup checks."),
        "hook findings should be inline, not a separate panel: {output}"
    );
    assert!(!output.contains("No command ran."), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("bash:"), "{output}");
}

#[test]
fn raw_cli_startup_banner_reports_selected_zsh_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_and_env(
        "fake",
        &["--shell", "zsh"],
        "exit\n",
        &[
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("TERM", "xterm-256color"),
        ],
    );

    assert!(output.contains("cosh-shell"), "{output}");
    assert!(output.contains("Shell: zsh"), "{output}");
    assert!(!output.contains("Shell: bash"), "{output}");
    assert!(!output.contains("zsh: command not found"), "{output}");
}

#[test]
fn raw_cli_shell_arg_can_select_zsh_raw_host() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_and_env(
        "fake",
        &["--shell", "zsh"],
        "echo zsh-cli:$ZSH_VERSION\nexit\n",
        &[("SHELL", "/bin/bash"), ("TERM", "xterm-256color")],
    );

    assert!(output.contains("zsh-cli:5"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("\x1b]1337;COSH;"), "{output}");
}

#[test]
fn raw_cli_unsupported_shell_reports_error_without_starting_bash() {
    assert_raw_cli_rejects_shell_args(
        &["raw", "fake", "--shell", "fish"],
        "unsupported raw shell: fish; supported shells: bash, zsh",
    );
}

#[test]
fn raw_cli_adapter_failure_keeps_shell_usable() {
    let output = run_raw_cli_with_input(
        "fake",
        "?? backend unavailable\n\
         echo after-backend-unavailable\n\
         exit 0\n",
    );

    assert!(output.contains("fake backend unavailable"), "{output}");
    assert!(output.contains("after-backend-unavailable"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
    assert!(
        !output.contains("The command ?? backend unavailable failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_backend_unavailable_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "?? backend unavailable\n\
         echo after-backend-unavailable\n\
         exit 0\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("正在思考..."), "{output}");
    assert!(output.contains("Agent 回复"), "{output}");
    assert!(output.contains("治理"), "{output}");
    assert!(output.contains("fake backend unavailable"), "{output}");
    assert!(output.contains("after-backend-unavailable"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("Agent response:"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
    assert!(
        !output.contains("The command ?? backend unavailable failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_adapter_error_keeps_shell_usable() {
    let output = run_raw_cli_with_input(
        "fake",
        "?? adapter crash\n\
         echo after-adapter-crash\n\
         exit 0\n",
    );

    assert!(output.contains("fake adapter crashed"), "{output}");
    assert!(output.contains("after-adapter-crash"), "{output}");
    assert!(!output.contains("bash: ??"), "{output}");
    assert!(
        !output.contains("The command ?? adapter crash failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_missing_shell_arg_reports_error_without_starting_bash() {
    assert_raw_cli_rejects_shell_args(
        &["raw", "fake", "--shell"],
        "missing value for --shell; supported shells: bash, zsh",
    );
}

fn first_health_prompt(output: &str) -> Option<String> {
    strip_ansi_escape(output)
        .lines()
        .find_map(|line| {
            let (_, prompt) = line.split_once("[Health]")?;
            Some(prompt.trim().trim_end_matches('│').trim().to_string())
        })
        .filter(|prompt| !prompt.is_empty())
}

fn compact_without_box_chars(output: &str) -> String {
    strip_ansi_escape(output)
        .chars()
        .map(|ch| {
            if matches!(ch, '│' | '╭' | '╰' | '─' | '╮' | '╯') {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn box_line_widths(output: &str) -> Vec<usize> {
    strip_ansi_escape(output)
        .replace('\r', "\n")
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_end();
            if trimmed.starts_with('╭') || trimmed.starts_with('╰') || trimmed.starts_with('│')
            {
                Some(terminal_display_width(trimmed))
            } else {
                None
            }
        })
        .collect()
}

fn terminal_display_width(line: &str) -> usize {
    Span::raw(line).width()
}

fn assert_raw_cli_rejects_shell_args(args: &[&str], expected: &str) {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run shell selection error case");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains(expected),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stdout.contains("cosh-osc$"),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("bash:"),
        "stdout={stdout}\nstderr={stderr}"
    );
}
