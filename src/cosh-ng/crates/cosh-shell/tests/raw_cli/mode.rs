use super::*;

#[test]
fn raw_cli_zsh_fragmented_mode_slash_does_not_accumulate_redraws() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        vec![
            (b"/m".to_vec(), Duration::ZERO),
            (b"o".to_vec(), Duration::from_millis(50)),
            (b"d".to_vec(), Duration::from_millis(50)),
            (b"e approval auto\n".to_vec(), Duration::from_millis(50)),
            (b"exit\n".to_vec(), Duration::from_millis(150)),
        ],
    );

    assert!(output.contains("Approval mode"), "{output}");
    assert!(output.contains("Mode set to auto."), "{output}");
    assert!(
        output.contains("Read-only tools auto-approved; risky requests need confirmation."),
        "{output}"
    );
    assert!(!output.contains("/m/mo"), "{output}");
    assert!(!output.contains("/mo/mod"), "{output}");
    assert!(!output.contains("/mod/mode"), "{output}");
    assert!(
        !output.contains("zsh: no such file or directory: /mode"),
        "{output}"
    );
}

#[test]
fn raw_cli_zsh_native_known_slash_does_not_reach_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_ISOLATED", "0")],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"echo after-native-mode\n".to_vec(),
                Duration::from_millis(150),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Approval mode"), "{output}");
    assert!(output.contains("Mode set to auto."), "{output}");
    assert!(output.contains("after-native-mode"), "{output}");
    assert!(
        !output.contains("zsh: no such file or directory: /mode"),
        "{output}"
    );
}

#[test]
fn raw_cli_zsh_native_pasted_mode_slash_does_not_reach_shell() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_ISOLATED", "0")],
        vec![
            (
                b"\x1b[200~/mode approval recommend\n\x1b[201~\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"echo after-native-pasted-mode\n".to_vec(),
                Duration::from_millis(200),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Approval mode"), "{output}");
    assert!(output.contains("Mode set to recommend."), "{output}");
    assert!(output.contains("after-native-pasted-mode"), "{output}");
    assert!(
        !output.contains("zsh: no such file or directory: /mode"),
        "{output}"
    );
}

#[test]
fn raw_cli_pasted_trust_confirm_sets_trust_mode_after_enter() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            (
                "cosh-osc$ ",
                b"\x1b[200~/mode approval trust confirm\n\x1b[201~\n",
            ),
            ("Mode set to trust.", b""),
            ("cosh-osc$ ", b"/help\n"),
            ("Mode: trust. Strategy: smart.", b"exit\n"),
        ],
    );

    assert!(output.contains("Approval mode"), "{output}");
    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Mode: trust. Strategy: smart."), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
}

#[test]
fn raw_cli_mode_rejects_quoted_arguments_without_changing_mode() {
    let output = run_raw_cli_with_input(
        "fake",
        "/mode approval \"trust confirm\"\n\
         /mode approval 'trust confirm'\n\
         /config language \"en US\"\n\
         /health \"quick\"\n\
         printf 'quoted-shell-output:%s\\n' '\"hello world\"'\n\
         /help\n\
         exit\n",
    );

    assert_eq!(
        count_occurrences(&output, "Quoted arguments are not supported."),
        4,
        "{output}"
    );
    assert!(
        output.contains("Use /mode approval trust confirm instead."),
        "{output}"
    );
    assert!(!output.contains("Unknown approval mode"), "{output}");
    assert!(!output.contains("Invalid language"), "{output}");
    assert!(!output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Mode: auto. Strategy: smart."), "{output}");
    assert!(
        output.contains("quoted-shell-output:\"hello world\""),
        "{output}"
    );
}

#[test]
fn raw_cli_mode_quoted_argument_error_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mode approval 'trust confirm'\n/help\nexit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("不支持带引号的参数。"), "{output}");
    assert!(
        output.contains("本例请改用 /mode approval trust confirm。"),
        "{output}"
    );
    assert!(!output.contains("未知审批模式"), "{output}");
    assert!(!output.contains("模式已设置为 trust。"), "{output}");
    assert!(output.contains("模式: auto. 策略: smart."), "{output}");
}

#[test]
fn raw_cli_help_and_mode_use_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/help\n/mode\n/mode language zh-CN\nexit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("Slash 命令"), "{output}");
    assert!(output.contains("配置"), "{output}");
    assert!(output.contains("配置界面语言"), "{output}");
    assert!(output.contains("审批: auto"), "{output}");
    assert!(output.contains("分析: smart"), "{output}");
    assert!(
        output.contains("语言是持久化配置，不是运行时模式。"),
        "{output}"
    );
    assert!(
        output.contains("使用 /config language [auto|en-US|zh-CN]。"),
        "{output}"
    );
    assert!(!output.contains("bash: /help"), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
    assert_no_migrated_english_ui_labels(&output, SLASH_CONFIG_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_mode_approval_and_analysis_use_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mode approval trust\n\
         /mode approval trust confirm\n\
         /mode analysis smart\n\
         /mode analysis auto\n\
         /mode analysis manual\n\
         exit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("需要确认 trust 模式"), "{output}");
    assert!(
        output.contains("运行 /mode approval trust confirm 显式启用。"),
        "{output}"
    );
    assert!(output.contains("模式已设置为 trust。"), "{output}");
    assert!(output.contains("分析模式"), "{output}");
    assert!(
        output.contains("命令失败或系统诊断输出有价值时评估；展示洞察供你复核。"),
        "{output}"
    );
    assert!(
        output.contains("仅对少量高置信故障自动触发 Agent 分析；其他情况仍先提示。"),
        "{output}"
    );
    let manual_footer = output
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        manual_footer
            .contains("已关闭被动建议、失败命令 Insight 和自动分析；使用 slash 命令手动触发。"),
        "{output}"
    );
    assert!(
        manual_footer.contains("个性化提示词推荐同时暂停，"),
        "{output}"
    );
    assert!(
        manual_footer.contains("可用 /recommendations 管理。"),
        "{output}"
    );
    assert!(!output.contains("bash: /mode"), "{output}");
    assert_no_migrated_english_ui_labels(&output, MODE_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_mode_approval_card_uses_zh_language_env() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"/mode approval\n"),
            ("\u{6309}\u{952e}: Left/Right \u{9009}\u{62e9}", b"\x1b[D\n"),
            (
                "\u{6a21}\u{5f0f}\u{5df2}\u{8bbe}\u{7f6e}\u{4e3a} recommend\u{3002}",
                b"",
            ),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("用户模式"), "{output}");
    assert!(output.contains("当前: auto"), "{output}");
    assert!(output.contains("只解释和建议"), "{output}");
    assert!(output.contains("按键: Left/Right 选择"), "{output}");
    assert!(output.contains("模式已设置为 recommend。"), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
    assert_no_migrated_english_ui_labels(&output, MODE_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_mode_analysis_card_selects_auto_for_current_session() {
    // Keep delayed input: the prompt repaints before card input ownership
    // returns to the shell, so a marker-gated `/help` right after the
    // analysis card can be silently dropped (input-ownership commit gap).
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"/mode analysis\n".to_vec(), Duration::from_millis(500)),
            (b"\x1b[C\n".to_vec(), Duration::from_millis(1_000)),
            (b"/help\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(200)),
        ],
    );

    assert!(output.contains("分析模式"), "{output}");
    assert!(output.contains("建议模式（推荐）"), "{output}");
    assert!(
        output.contains("自动分析（命令失败后可能自动启动 Agent）"),
        "{output}"
    );
    assert!(output.contains("关闭主动介入"), "{output}");
    assert!(output.contains("模式已设置为 auto。"), "{output}");
    assert!(output.contains("模式: auto. 策略: auto."), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
}

#[test]
fn raw_cli_mode_analysis_card_cancel_keeps_current_session_mode() {
    // Keep delayed input: same input-ownership commit gap as the
    // selects-auto case above.
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"/mode analysis\n".to_vec(), Duration::from_millis(500)),
            (b"\x03".to_vec(), Duration::from_millis(1_000)),
            (b"/help\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(200)),
        ],
    );

    assert!(output.contains("分析模式"), "{output}");
    assert!(output.contains("模式未改变: smart。"), "{output}");
    assert!(output.contains("模式: auto. 策略: smart."), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
}

#[test]
fn raw_cli_mode_slash_updates_approval_mode_with_feedback() {
    let output = run_raw_cli_with_input(
        "fake",
        "/mode approval trust\n\
         /mode approval trust confirm\n\
         /help\n\
         /approval-mode recommend\n\
         /mode auto\n\
         /mode approval invalid\n\
         /mode invalid\n\
         echo after-mode\n\
         exit\n",
    );

    assert!(output.contains("Approval mode"), "{output}");
    assert!(output.contains("Trust confirmation required"), "{output}");
    assert!(
        output.contains("Run /mode approval trust confirm"),
        "{output}"
    );
    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Mode: trust. Strategy: smart."), "{output}");
    assert!(
        output.contains("/approval-mode is not supported."),
        "{output}"
    );
    assert!(output.contains("Use /mode approval recommend."), "{output}");
    assert!(output.contains("/mode auto is not supported."), "{output}");
    assert!(output.contains("Use /mode approval auto."), "{output}");
    assert!(!output.contains("Mode set to recommend."), "{output}");
    assert!(!output.contains("Mode set to auto."), "{output}");
    assert!(output.contains("Unknown mode: invalid"), "{output}");
    assert!(
        output.contains("Use /mode approval recommend|auto|trust"),
        "{output}"
    );
    assert!(output.contains("after-mode"), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
    assert!(!output.contains("bash: /approval-mode"), "{output}");
}

#[test]
fn raw_cli_mode_root_and_language_guidance_are_canonical() {
    let output = run_raw_cli_with_input(
        "fake",
        "/mode\n\
         /mode language zh-CN\n\
         echo after-mode-guidance\n\
         exit\n",
    );

    assert!(output.contains("Modes"), "{output}");
    assert!(output.contains("approval: auto"), "{output}");
    assert!(output.contains("analysis: smart"), "{output}");
    assert!(
        output.contains("Use /mode approval, /mode analysis, or /mode routing"),
        "{output}"
    );
    assert!(
        output.contains("Language is persistent config, not a runtime mode."),
        "{output}"
    );
    assert!(
        output.contains("Use /config language [auto|en-US|zh-CN]."),
        "{output}"
    );
    assert!(output.contains("after-mode-guidance"), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
}

#[test]
fn raw_cli_mode_slash_panel_selects_recommend_with_card_input() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"/mode approval\n"),
            ("> [ auto", b"\x1b[D\n"),
            ("Mode set to recommend.", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("User mode"), "{output}");
    assert!(output.contains("Current: auto"), "{output}");
    assert!(output.contains("> [ auto"), "{output}");
    assert!(output.contains("Mode set to recommend."), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
    assert!(!output.contains("bash: \u{1b}"), "{output}");
}

#[test]
fn raw_cli_suggest_mode_keeps_tool_requests_display_only() {
    // Keep delayed input here: in recommend mode `??` routes into the
    // tool-details flow whose session teardown races marker-gated stdin.
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[],
        vec![
            (b"/mode approval recommend\n".to_vec(), Duration::ZERO),
            (
                b"?? request tool approval\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );

    assert!(output.contains("Mode set to recommend."), "{output}");
    assert!(output.contains("Received shell prompt request"), "{output}");
    assert!(!output.contains("Approval req-"), "{output}");
    assert!(!output.contains("Auto-approved"), "{output}");
    assert!(
        !output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
}

#[test]
fn raw_cli_auto_mode_runs_safe_bash_tool_without_approval_panel() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mode approval auto\n\
         ?? request tool approval\n\
         exit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    assert!(output.contains("Mode set to auto."), "{output}");
    assert!(output.contains("Deferred req-1"), "{output}");
    assert!(output.contains("$ git status"), "{output}");
    assert!(!output.contains("Approval req-1"), "{output}");
    assert!(!output.contains("[ Allow once ]"), "{output}");
    assert!(!output.contains("Command result analysis for req-1"));
    assert!(!output.contains("Tool result for request req-1"));
    assert!(
        !output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
}

#[test]
fn raw_cli_trust_mode_runs_bash_tool_without_approval_panel() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"/mode approval trust confirm\n"),
            ("Mode set to trust.", b"?? stream pwd tool approval\n"),
            ("Deferred req-1", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Mode set to trust."), "{output}");
    assert!(output.contains("Deferred req-1"), "{output}");
    assert!(!output.contains("Command result analysis for req-1"));
    assert!(!output.contains("Tool result for request req-1"));
    assert!(!output.contains("Approval req-"), "{output}");
    assert!(!output.contains("[ Allow once ]"), "{output}");
}

#[test]
fn raw_cli_auto_mode_skips_readonly_builtin_tool_approval_panel() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? request readonly builtin tool\n".to_vec(),
                Duration::from_millis(200),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );

    assert!(output.contains("Mode set to auto."), "{output}");
    assert!(!output.contains("Auto-approved req-"), "{output}");
    assert!(!output.contains("Read called"), "{output}");
    assert!(!output.contains("Grep called"), "{output}");
    assert!(!output.contains("[Details] tool-"), "{output}");
    assert!(!output.contains("Approval req-1"), "{output}");
    assert!(!output.contains("[ Allow once ]"), "{output}");
    assert!(!output.contains("$ {\"file_path\""), "{output}");
}

#[test]
fn raw_cli_auto_mode_still_asks_for_unsafe_bash_tool() {
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"/mode approval auto\n"),
            ("Mode set to auto.", b"?? request unsafe tool approval\n"),
            ("req-1", b"\x1b"),
            ("Cancelled req-1", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Mode set to auto."), "{output}");
    assert_approval_prompt_visible(&output);
    assert!(output.contains("req-1"), "{output}");
    assert!(
        output.contains("touch /tmp/cosh-shell-fake-action-should-not-run"),
        "{output}"
    );
    assert!(output.contains("Cancelled req-1"), "{output}");
    assert!(!output.contains("Auto-approved"), "{output}");
    assert!(!output.contains("Approved req-1"), "{output}");
}

#[test]
fn raw_cli_auto_mode_skips_exact_trusted_command() {
    let home = temp_shell_home("trusted-exact");
    write_cosh_config(
        &home,
        r#"shell.trusted_command = "touch /tmp/cosh-shell-fake-action-should-not-run""#,
    );
    let home_str = home.to_string_lossy().to_string();
    let _ = fs::remove_file("/tmp/cosh-shell-fake-action-should-not-run");

    let output = run_raw_cli_with_env(
        "fake",
        "?? request unsafe tool approval\nexit\n",
        &[("HOME", &home_str)],
    );

    assert!(output.contains("Deferred req-1"), "{output}");
    assert!(output.contains("$ touch /tmp/cosh-shell-fake-action-should-not-run"));
    assert!(!output.contains("Approval req-1"), "{output}");
    assert!(!output.contains("Trusted req-1"), "{output}");

    let _ = fs::remove_file("/tmp/cosh-shell-fake-action-should-not-run");
}

#[test]
fn raw_cli_auto_mode_trusted_command_requires_exact_match() {
    let home = temp_shell_home("trusted-exact-miss");
    write_cosh_config(
        &home,
        r#"shell.trusted_command = "touch /tmp/cosh-shell-fake-action-should-not-run --dry-run""#,
    );
    let home_str = home.to_string_lossy().to_string();
    let _ = fs::remove_file("/tmp/cosh-shell-fake-action-should-not-run");

    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[("HOME", &home_str)],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"?? request unsafe tool approval\n"),
            ("req-1", b"\x1b"),
            ("Cancelled req-1", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert_approval_prompt_visible(&output);
    assert!(!output.contains("Trusted req-1"), "{output}");
    assert!(!output.contains("Auto-approved req-1"), "{output}");

    let _ = fs::remove_file("/tmp/cosh-shell-fake-action-should-not-run");
}
