use super::*;

#[test]
fn raw_cli_failure_insight_keeps_shell_only_ownership_of_shift_tab() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        vec![
            (b"\x1b[Z".to_vec(), Duration::from_millis(500)),
            (
                b"cosh-command-that-does-not-exist\n".to_vec(),
                Duration::from_millis(200),
            ),
            (b"\x1b[Z".to_vec(), Duration::from_millis(1_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let visible = strip_ansi_escape(&output);

    assert!(
        visible.contains("cosh-command-that-does-not-exist: command not found"),
        "{output}"
    );
    assert!(
        visible.contains("The previous input did not run successfully"),
        "{output}"
    );
    assert!(count_occurrences(&visible, "◌ ") >= 2, "{output}");
    assert_eq!(count_occurrences(&visible, "◇ "), 1, "{output}");
}

#[test]
fn raw_cli_failure_insight_keeps_assisted_ownership_of_shift_tab() {
    let home = temp_shell_home("assistance-disable-over-insight");
    fs::write(home.join(".bashrc"), "PS1='insight-owner$ '\n").unwrap();
    let home_str = home.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        vec![
            (
                b"cosh-command-that-does-not-exist\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\x1b[Z".to_vec(), Duration::from_millis(1_000)),
            (
                b"printf '__SHELL_ONLY__\\n'\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output);

    assert!(
        visible.contains("cosh-command-that-does-not-exist: command not found"),
        "{output}"
    );
    assert!(
        visible.contains("The previous input did not run successfully"),
        "{output}"
    );
    assert!(
        visible.contains("insight-owner$ printf '__SHELL_ONLY__\\n'"),
        "{output}"
    );
    assert!(!visible.contains("◌ "), "{output}");
}

#[test]
fn raw_cli_success_then_command_not_found_keeps_failure_insight() {
    let home = temp_shell_home("success-then-command-not-found-insight");
    fs::write(home.join(".bashrc"), "PS1='insight-sequence$ '\n").unwrap();
    let home_str = home.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("COSH_SHELL_INTEGRATION", "enhanced"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        vec![
            (
                b"printf '__PREVIOUS_OK__\\n'\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"sdsd\n".to_vec(), Duration::from_millis(300)),
            (b"exit 0\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let visible = strip_ansi_escape(&output);

    assert!(visible.contains("__PREVIOUS_OK__"), "{output}");
    assert!(visible.contains("sdsd: command not found"), "{output}");
    assert!(
        visible.contains("The previous input did not run successfully"),
        "{output}"
    );
}

#[test]
fn raw_cli_slash_after_failed_command_invokes_adapter() {
    let output = run_raw_cli_with_env(
        "fake",
        "ls /path/that/does/not/exist\n/explain last error\necho after-explain\nexit 0\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    assert_agent_loading_visible(&output);
    assert!(output.contains("The command ls /path/that/does/not/exist failed"));
    assert_inline_before_followup(&output, "Thinking...", "The command");
    assert_inline_before_followup(&output, "The command", "after-explain");
    assert!(!output.contains("[Analyze] [Dismiss]"), "{output}");
    assert!(!output.contains("[Details] cmd-"), "{output}");
}

#[test]
fn raw_cli_smart_usage_help_failure_does_not_start_agent() {
    let fixture = temp_shell_home("usage-help-smart");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("demo-usage"),
        "#!/bin/sh\n\
         echo \"error: unexpected argument '$1'\" >&2\n\
         echo \"Usage: demo-usage [OPTIONS]\" >&2\n\
         echo \"Try 'demo-usage --help' for more information.\" >&2\n\
         exit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run_raw_cli_with_env(
        "fake",
        "demo-usage --bad\necho after-usage\nexit\n",
        &[("COSH_SHELL_LANG", "en-US"), ("PATH", path.as_str())],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(output.contains("Usage: demo-usage [OPTIONS]"), "{output}");
    assert!(output.contains("after-usage"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(
        !output.contains("The command demo-usage --bad failed"),
        "{output}"
    );
    assert!(!output.contains("Command failed"), "{output}");
}

#[test]
fn raw_cli_explain_after_usage_help_failure_invokes_adapter() {
    let fixture = temp_shell_home("usage-help-explain");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("demo-usage"),
        "#!/bin/sh\n\
         echo \"error: unexpected argument '$1'\" >&2\n\
         echo \"Usage: demo-usage [OPTIONS]\" >&2\n\
         echo \"Try 'demo-usage --help' for more information.\" >&2\n\
         exit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run_raw_cli_with_env(
        "fake",
        "demo-usage --bad\n/explain last error\necho after-explain\nexit\n",
        &[("COSH_SHELL_LANG", "en-US"), ("PATH", path.as_str())],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert_agent_loading_visible(&output);
    assert!(
        output.contains("The command demo-usage --bad failed"),
        "{output}"
    );
    assert_inline_before_followup(&output, "The command", "after-explain");
}

#[test]
fn raw_cli_build_failure_respects_analysis_mode_matrix() {
    for (mode, expects_insight, expects_agent) in [
        ("smart", true, false),
        ("auto", false, true),
        ("manual", false, false),
    ] {
        let fixture = temp_shell_home(&format!("build-failure-{mode}"));
        let bin_dir = fixture.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable(
            &bin_dir.join("make"),
            "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
        );
        let path = format!(
            "{}:{}",
            bin_dir.to_string_lossy(),
            std::env::var("PATH").unwrap_or_default()
        );
        let analysis_marker = match mode {
            "smart" => "Insight: The build or test command failed",
            "auto" => "The command make all failed with exit code 2.",
            "manual" => "make: *** [all] Error 2",
            _ => unreachable!(),
        };
        let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
            "fake",
            &[],
            &[
                ("COSH_SHELL_LANG", "en-US"),
                ("COSH_SHELL_ANALYSIS_MODE", mode),
                ("PATH", path.as_str()),
            ],
            Path::new(env!("CARGO_MANIFEST_DIR")),
            &[
                ("cosh-osc$ ", b"make all\n"),
                (analysis_marker, b""),
                ("cosh-osc$ ", b"echo after-build\nexit\n"),
            ],
        );
        let _ = fs::remove_dir_all(&fixture);

        assert_eq!(
            output.contains("Insight: The build or test command failed"),
            expects_insight,
            "{mode}: {output}"
        );
        assert_eq!(
            output.contains("The command make all failed with exit code 2."),
            expects_agent,
            "{mode}: {output}"
        );
        assert!(!output.contains("[Analyze] [Dismiss]"), "{mode}: {output}");
        assert!(!output.contains("[Details] cmd-"), "{mode}: {output}");
        assert!(!output.contains("id: cmd-"), "{mode}: {output}");
        assert!(output.contains("after-build"), "{mode}: {output}");
    }
}

#[test]
fn raw_cli_runtime_exception_requires_confirmation_outside_manual_mode() {
    for (mode, expects_insight) in [("smart", true), ("auto", true), ("manual", false)] {
        let fixture = temp_shell_home(&format!("runtime-exception-{mode}"));
        let bin_dir = fixture.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable(
            &bin_dir.join("demo-runtime"),
            "#!/bin/sh\n\
             echo 'Traceback (most recent call last):' >&2\n\
             echo '  File \"app.py\", line 1, in <module>' >&2\n\
             echo 'ValueError: boom' >&2\n\
             exit 1\n",
        );
        let path = format!(
            "{}:{}",
            bin_dir.to_string_lossy(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = run_raw_cli_with_args_env_and_delayed_input(
            "fake",
            &[],
            &[
                ("COSH_SHELL_LANG", "en-US"),
                ("COSH_SHELL_ANALYSIS_MODE", mode),
                ("PATH", path.as_str()),
            ],
            vec![
                (b"demo-runtime\n".to_vec(), Duration::ZERO),
                (
                    b"echo after-runtime\nexit\n".to_vec(),
                    Duration::from_millis(800),
                ),
            ],
        );
        let _ = fs::remove_dir_all(&fixture);

        assert_eq!(
            output.contains("Insight: The program terminated with an unhandled exception"),
            expects_insight,
            "{mode}: {output}"
        );
        assert!(
            !output.contains("Agent analysis is starting."),
            "{mode}: {output}"
        );
        assert!(
            !output.contains("The command demo-runtime failed with exit code 1."),
            "{mode}: {output}"
        );
        assert!(output.contains("after-runtime"), "{mode}: {output}");
    }
}

#[test]
fn raw_cli_auto_repeated_build_failure_is_suppressed_without_old_notice() {
    let fixture = temp_shell_home("repeated-build-failure");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("make"),
        "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
            ("PATH", path.as_str()),
        ],
        vec![
            (b"make all\n".to_vec(), Duration::ZERO),
            (b"make all\n".to_vec(), Duration::from_millis(800)),
            (
                b"echo after-repeat\nexit\n".to_vec(),
                Duration::from_millis(800),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert_eq!(
        count_occurrences(&output, "The command make all failed with exit code 2."),
        1,
        "{output}"
    );
    assert!(!output.contains("Analysis skipped"), "{output}");
    assert!(!output.contains("[Analyze] [Dismiss]"), "{output}");
    assert!(output.contains("after-repeat"), "{output}");
}

#[test]
fn raw_cli_zh_auto_build_failure_localizes_auto_analyze_activity() {
    let fixture = temp_shell_home("zh-auto-build-failure");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("make"),
        "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "zh-CN"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
            ("PATH", path.as_str()),
        ],
        vec![
            (b"make all\n".to_vec(), Duration::ZERO),
            (
                b"echo after-zh-auto\nexit\n".to_vec(),
                Duration::from_millis(800),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(output.contains("`make all` 退出码为 2"), "{output}");
    assert!(output.contains("Agent 分析正在启动。"), "{output}");
    assert!(
        output.contains("命令 make all 以退出码 2 失败。"),
        "{output}"
    );
    assert!(!output.contains("The command make all failed"), "{output}");
    assert!(!output.contains("Agent analysis is starting."), "{output}");
    assert!(output.contains("after-zh-auto"), "{output}");
}

#[test]
fn raw_cli_plain_smart_insight_has_no_insight_ansi() {
    let fixture = temp_shell_home("plain-smart-build-insight");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("make"),
        "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("COSH_SHELL_RENDER", "plain"),
            ("PATH", path.as_str()),
        ],
        vec![
            (b"make all\n".to_vec(), Duration::ZERO),
            (
                b"echo after-plain\nexit\n".to_vec(),
                Duration::from_millis(800),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(
        output.contains(
            "Insight: The build or test command failed  Press Tab to fill, then Enter to submit; keep typing to ignore"
        ),
        "{output}"
    );
    assert!(!output.contains("\x1b[36mInsight:"), "{output}");
    assert!(!output.contains("\x1b[2mTab to fill"), "{output}");
    assert!(output.contains("after-plain"), "{output}");
}

#[test]
fn raw_cli_smart_build_failure_ghost_tab_then_enter_starts_bound_analysis() {
    let fixture = temp_shell_home("smart-build-ghost");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("make"),
        "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("PATH", path.as_str()),
        ],
        vec![
            (b"make all\n".to_vec(), Duration::ZERO),
            (vec![b'\t'], Duration::from_millis(3_000)),
            (b"\n".to_vec(), Duration::from_millis(200)),
            (
                b"echo after-ghost\nexit\n".to_vec(),
                Duration::from_millis(800),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(
        output.contains("Insight: The build or test command failed"),
        "{output}"
    );
    assert!(
        output.contains("Press Tab to fill, then Enter to submit; keep typing to ignore"),
        "{output}"
    );
    assert!(
        output.contains("The command make all failed with exit code 2."),
        "{output}"
    );
    assert!(
        !output.contains("Received shell prompt request"),
        "{output}"
    );
    assert!(!output.contains("prompt_ghost:"), "{output}");
    assert!(!output.contains("[Analyze] [Dismiss]"), "{output}");
    assert!(output.contains("after-ghost"), "{output}");
}

#[test]
fn raw_cli_smart_agent_prompt_split_arrow_cancels_without_shell_leak() {
    let fixture = temp_shell_home("smart-build-split-arrow");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("make"),
        "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANALYSIS_MODE", "smart"),
            ("PATH", path.as_str()),
        ],
        vec![
            (b"make all\n".to_vec(), Duration::ZERO),
            (vec![b'\t'], Duration::from_millis(3_000)),
            (vec![0x1b], Duration::from_millis(200)),
            (b"[".to_vec(), Duration::from_millis(50)),
            (b"D".to_vec(), Duration::from_millis(50)),
            (
                b"echo after-split-arrow\nexit\n".to_vec(),
                Duration::from_millis(300),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(
        output.contains("Insight: The build or test command failed"),
        "{output}"
    );
    assert!(output.contains("after-split-arrow"), "{output}");
    assert!(
        !output.contains("The command make all failed with exit code 2."),
        "{output}"
    );
    assert!(
        !output.contains("analyze this build or test failure: command not found"),
        "{output}"
    );
}

#[test]
fn raw_cli_smart_command_not_found_is_silent_without_unique_rewrite() {
    let output = run_raw_cli_with_env(
        "fake",
        "cosh_missing_command_for_failure_policy\necho after-missing\nexit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    assert!(output.contains("command not found"), "{output}");
    assert!(!output.contains("Command failed"), "{output}");
    assert!(!output.contains("[Analyze] [Dismiss]"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
    assert!(!output.contains("id: cmd-"), "{output}");
    assert!(output.contains("after-missing"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(
        !output.contains("The command cosh_missing_command_for_failure_policy failed"),
        "{output}"
    );
}

#[test]
fn raw_cli_clear_keeps_generic_failure_silent() {
    let output = run_raw_cli_with_env(
        "fake",
        "ls /path/that/does/not/exist\n/clear\n/explain last error\necho after-clear\nexit 0\n",
        &[("COSH_SHELL_ANALYSIS_MODE", "auto")],
    );

    assert!(!output.contains("The command ls /path/that/does/not/exist failed"));
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("Command failed:"), "{output}");
    assert!(output.contains("after-clear"));
}

#[test]
fn raw_cli_shell_keeps_generic_failure_silent() {
    let output = run_raw_cli_with_env(
        "fake",
        "ls /path/that/does/not/exist\n/shell\n/explain last error\necho after-shell\nexit 0\n",
        &[("COSH_SHELL_ANALYSIS_MODE", "auto")],
    );

    assert!(!output.contains("The command ls /path/that/does/not/exist failed"));
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("Command failed:"), "{output}");
    assert!(output.contains("after-shell"));
}

#[test]
fn raw_cli_natural_language_does_not_make_later_generic_failure_actionable() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_ANALYSIS_MODE", "auto")],
        vec![
            ("\u{4f60}\u{597d}\n".as_bytes().to_vec(), Duration::ZERO),
            (
                b"ls /path/that/does/not/exist\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert_agent_loading_visible(&output);
    assert!(output.contains("Received shell prompt request"));
    assert!(!output.contains("The command ls /path/that/does/not/exist failed"));
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_natural_language_omits_unbound_recent_failed_command_context() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"ls /path/that/does/not/exist\n"),
            ("No such file or directory", b"please show context\n"),
            ("Recent context visible to Agent", b"exit\n"),
        ],
    );

    assert!(
        output.contains("Recent context visible to Agent"),
        "{output}"
    );
    let compact = compact_terminal_words(&output);
    assert!(
        compact.contains("Runtime context hints visible to Agent: <none>"),
        "{output}"
    );
    assert!(
        compact.contains("Recent context visible to Agent: <none>"),
        "{output}"
    );
    assert!(
        !compact.contains("command=ls /path/that/does/not/exist"),
        "{output}"
    );
    assert!(
        !compact.contains("output_id=terminal-output://raw-session/cmd-1"),
        "{output}"
    );
    assert!(!output.contains("hook-cmd-"), "{output}");
    assert!(!output.contains("ref="), "{output}");
    assert!(
        !output.contains("No command ran; Agent actions still require governance."),
        "{output}"
    );
}

#[test]
fn raw_cli_natural_language_after_failure_does_not_bind_generic_failure() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"ls /path/that/does/not/exist\n".to_vec(), Duration::ZERO),
            (
                "\u{4f60}\u{597d}\n".as_bytes().to_vec(),
                Duration::from_millis(400),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(400)),
        ],
    );

    assert_agent_loading_visible(&output);
    assert!(output.contains("Received shell prompt request: \u{4f60}\u{597d}"));
    assert!(!output.contains("The command ls /path/that/does/not/exist failed"));
    assert!(!output.contains("Command failed:"), "{output}");
}

#[test]
fn raw_cli_generic_failure_is_silent_before_next_prompt() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"ls ccc\n".to_vec(), Duration::ZERO),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("No such file or directory"), "{output}");
    assert!(ls_ccc_failure_analysis(&output).is_none(), "{output}");
    assert!(!output.contains("Insight:"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("Command failed:"), "{output}");
    assert!(!output.contains("Agent not called"));
    assert!(!output.contains("suggestion: show a short explanation"));
    assert!(!output.contains("`exit` exited with code"));
    assert!(!output.contains("The command exit failed"));
    assert!(!output.contains("Approval not found"), "{output}");
}

#[test]
fn raw_cli_repeated_generic_failure_stays_silent_without_budget_notice() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_ANALYSIS_MODE", "auto")],
        vec![
            (b"ls ccc\n".to_vec(), Duration::ZERO),
            (b"ls ccc\n".to_vec(), Duration::from_millis(800)),
            (
                b"echo after-repeat\nexit\n".to_vec(),
                Duration::from_millis(800),
            ),
        ],
    );

    assert!(!output.contains("Analysis skipped"), "{output}");
    assert!(ls_ccc_failure_analysis(&output).is_none(), "{output}");
    assert!(output.contains("after-repeat"), "{output}");
    assert!(!output.contains("[Analyze] [Ignore]"), "{output}");
}

#[test]
fn raw_cli_zh_repeated_generic_failure_stays_silent() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_SHELL_LANG", "zh-CN"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"ls ccc\n".to_vec(), Duration::ZERO),
            (b"ls ccc\n".to_vec(), Duration::from_millis(800)),
            (
                b"echo after-zh-repeat\nexit\n".to_vec(),
                Duration::from_millis(800),
            ),
        ],
    );

    assert!(!output.contains("已跳过分析"), "{output}");
    assert!(!output.contains("Agent 回复"), "{output}");
    assert!(ls_ccc_failure_analysis(&output).is_none(), "{output}");
    assert!(output.contains("after-zh-repeat"), "{output}");
    assert!(!output.contains("bash: ls ccc"), "{output}");
}

#[test]
fn raw_cli_hook_consultation_uses_zh_language_env() {
    let fixture = temp_shell_home("hook-consultation-zh");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("free"),
        "#!/bin/sh\ncat <<'EOF'\n              total        used        free      shared  buff/cache   available\nMem:          32768       30200         380          16        2188        1400\nSwap:          8192        4096        4096\nEOF\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN"), ("PATH", path.as_str())],
        vec![
            (b"free -m\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(1200)),
        ],
    );

    assert!(output.contains("洞察：当前内存压力需要关注"), "{output}");
    assert!(!output.contains("Available memory is low"), "{output}");
    assert!(!output.contains("[分析] [忽略]"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
    assert!(!output.contains("Hook: memory-pressure"), "{output}");
    assert!(
        !output.contains("置信度: medium; 原因: allowed"),
        "{output}"
    );
    assert!(!output.contains("Confidence:"), "{output}");
    assert!(!output.contains("reason:"), "{output}");
    assert!(!output.contains("[Analyze] [Ignore]"), "{output}");
    assert!(!output.contains("bash: free -m"), "{output}");
}

#[test]
fn raw_cli_repeated_ps_dash_aux_generic_failure_stays_silent() {
    let fixture = temp_shell_home("ps-dash-aux");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let ps_path = bin_dir.join("ps");
    fs::write(
        &ps_path,
        "#!/bin/sh\nif [ \"$1\" = \"-aux\" ]; then\n  echo \"ps: No user named 'x'\" >&2\n  exit 1\nfi\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&ps_path, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("PATH", path.as_str()),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"ps -aux\n".to_vec(), Duration::ZERO),
            (b"ps -aux\n".to_vec(), Duration::from_millis(1200)),
            (
                b"echo after-ps-repeat\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(!output.contains("Analysis skipped"), "{output}");
    assert!(
        !output.contains("The command ps -aux failed with exit code 1."),
        "{output}"
    );
    assert!(output.contains("after-ps-repeat"), "{output}");
    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("[Analyze] [Ignore]"), "{output}");
    assert!(!output.contains("Approval not found"), "{output}");
}

#[test]
fn raw_cli_tail_follow_ctrl_c_does_not_start_agent_analysis() {
    let output = run_raw_cli_with_input(
        "fake",
        "bash -c 'tail -f /dev/null & BGPID=$!; sleep 0.2; kill $BGPID; wait $BGPID 2>/dev/null'\necho after-tail-follow\nexit\n",
    );

    assert!(output.contains("after-tail-follow"), "{output}");
    assert!(!output.contains("Command hook"), "{output}");
    assert!(!output.contains("Command result finding"), "{output}");
}

#[test]
fn raw_cli_generic_failure_does_not_trigger_inline_guidance() {
    let output = run_raw_cli_with_envs(
        "fake",
        &[
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
    );

    assert!(!output.contains("Thinking..."));
    assert!(!output.contains("Agent status"));
    assert!(!output.contains("Phase: analyzing"));
    assert!(!output.contains("The command ls /path/that/does/not/exist failed"));
    assert!(output.contains("after-inline"), "{output}");
}

#[test]
fn raw_cli_zsh_generic_failure_restores_prompt_without_insight() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("failed-hook-prompt");
    fs::write(home.join(".zshrc"), "PROMPT='ZPROMPT> '\nRPROMPT=''\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_ANALYSIS_MODE", "auto"),
        ],
        vec![
            (b"ls ccc\n".to_vec(), Duration::ZERO),
            (
                b"echo after-hook\nexit\n".to_vec(),
                Duration::from_millis(1200),
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(!output.contains("Thinking..."), "{output}");
    assert!(!output.contains("[Analyze] [Ignore]"), "{output}");
    assert!(!output.contains("Insight:"), "{output}");
    assert!(output.contains("after-hook"), "{output}");
    assert!(
        count_occurrences_between(
            &output,
            "No such file or directory",
            "echo after-hook",
            "ZPROMPT> "
        ) >= 1,
        "{output}"
    );
}

#[test]
fn raw_cli_isolated_bash_smart_and_auto_restore_one_prompt() {
    for mode in ["smart", "auto"] {
        assert_failed_command_prompt_restore("bash", true, mode, &[]);
    }
}

#[test]
fn raw_cli_isolated_zsh_smart_and_auto_restore_one_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh unavailable; mandatory coverage runs in the amd64 Anolis gate");
        return;
    }
    for mode in ["smart", "auto"] {
        assert_failed_command_prompt_restore("zsh", true, mode, &[]);
    }
}

#[test]
fn raw_cli_normal_bash_smart_and_auto_restore_one_prompt() {
    for mode in ["smart", "auto"] {
        assert_failed_command_prompt_restore("bash", false, mode, &[]);
    }
}

#[test]
fn raw_cli_normal_zsh_smart_and_auto_restore_one_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh unavailable; mandatory coverage runs in the amd64 Anolis gate");
        return;
    }
    for mode in ["smart", "auto"] {
        assert_failed_command_prompt_restore("zsh", false, mode, &[]);
    }
}

#[test]
fn raw_cli_isolated_bash_auto_plain_narrow_restores_one_prompt() {
    assert_failed_command_prompt_restore(
        "bash",
        true,
        "auto",
        &[
            ("COSH_SHELL_RENDER", "plain"),
            ("NO_COLOR", "1"),
            ("COLUMNS", "40"),
        ],
    );
}

fn assert_failed_command_prompt_restore(
    shell: &str,
    isolated: bool,
    mode: &str,
    extra_env: &[(&str, &str)],
) {
    let fixture = temp_shell_home(&format!("prompt-restore-{shell}-{isolated}-{mode}"));
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("make"),
        "#!/bin/sh\necho 'make: *** [all] Error 2' >&2\nexit 2\n",
    );
    let sentinel = format!(
        "PROMPT_{shell}_{mode}_{}> ",
        if isolated { "I" } else { "N" }
    );
    if !isolated {
        let rc = if shell == "zsh" { ".zshrc" } else { ".bashrc" };
        let prompt_var = if shell == "zsh" { "PROMPT" } else { "PS1" };
        fs::write(
            fixture.join(rc),
            format!("{prompt_var}='{sentinel}'\nRPROMPT=''\n"),
        )
        .unwrap();
    }
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );
    let home = fixture.to_string_lossy().into_owned();
    let isolated_value = if isolated { "1" } else { RAW_CLI_UNSET_ENV };
    let mut input = vec![(b"make all\n".to_vec(), Duration::ZERO)];
    if mode == "smart" {
        input.push((b"\t\n".to_vec(), Duration::from_millis(2800)));
    }
    input.push((
        b"echo PROMPT_FLOW_OK\nexit\n".to_vec(),
        Duration::from_millis(1800),
    ));
    let mut env = vec![
        ("HOME", home.as_str()),
        ("PATH", path.as_str()),
        ("COSH_SHELL_LANG", "en-US"),
        ("COSH_SHELL_ANALYSIS_MODE", mode),
        ("COSH_SHELL_STARTUP_BANNER", "0"),
        ("COSH_SHELL_ISOLATED", isolated_value),
        ("COSH_POC_PS1", sentinel.as_str()),
    ];
    env.extend_from_slice(extra_env);
    let output =
        run_raw_cli_with_args_env_and_delayed_input("fake", &["--shell", shell], &env, input);
    let _ = fs::remove_dir_all(&fixture);

    let normalized = strip_ansi_escape(&output);
    let result_marker = "failed with exit code 2.";
    assert!(normalized.contains("PROMPT_FLOW_OK"), "{output}");
    assert!(
        normalized.contains(result_marker),
        "shell={shell} isolated={isolated} mode={mode}\n{output}"
    );
    assert!(
        normalized.contains("echo PROMPT_FLOW_OK"),
        "shell={shell} isolated={isolated} mode={mode}\n{output}"
    );
    assert_eq!(
        count_occurrences_between(&normalized, result_marker, "echo PROMPT_FLOW_OK", &sentinel,),
        1,
        "shell={shell} isolated={isolated} mode={mode}\n{output}"
    );
}
