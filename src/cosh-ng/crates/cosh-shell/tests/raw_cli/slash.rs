use super::*;

#[test]
fn raw_cli_help_renders_slash_command_reference() {
    // Pin the terminal type: the rich-border assertions below depend on the
    // non-plain render path, which TERM=dumb hosts would downgrade.
    let output = run_raw_cli_with_env(
        "fake",
        "/help\necho after-help\nexit\n",
        &[("TERM", "xterm-256color")],
    );
    let normalized = strip_ansi_escape(&output);

    assert!(normalized.contains("Slash commands"), "{output}");
    // Group headers sit at column zero inside the panel.
    assert!(normalized.contains("│ Config"), "{output}");
    assert!(normalized.contains("│ Status"), "{output}");
    assert!(normalized.contains("│ Modes"), "{output}");
    assert!(normalized.contains("│ Hooks"), "{output}");
    assert!(normalized.contains("│ Registry"), "{output}");
    // Entries keep a two-space indent below their group header.
    assert!(normalized.contains("│   /config language"), "{output}");
    // Scope tags are right-aligned against the panel border.
    assert!(normalized.contains("[config] │"), "{output}");
    // Summaries sit on their own indented line.
    assert!(
        normalized.contains("│       configure UI language"),
        "{output}"
    );
    assert!(!normalized.contains("Inspect"), "{output}");
    assert!(!normalized.contains("Recommendations"), "{output}");
    assert!(
        normalized.contains("/config language [auto|en-US|zh-CN]"),
        "{output}"
    );
    assert!(normalized.contains("/status"), "{output}");
    // /auth renders as an indented group entry with its own summary line; the
    // contextual /audit stays hidden below. Config-group membership is pinned
    // by the registry unit test recommendations_and_auth_are_public_config_controls.
    assert!(normalized.contains("│   /auth"), "{output}");
    assert!(
        normalized.contains("│       configure AI provider credentials"),
        "{output}"
    );
    assert!(normalized.contains("/stats [model|tools]"), "{output}");
    assert!(
        normalized.contains("/mode approval [recommend|auto|trust]"),
        "{output}"
    );
    assert!(
        normalized.contains("/mode analysis [smart|auto|manual]"),
        "{output}"
    );
    assert!(
        normalized.contains("/extensions <command> [options]"),
        "{output}"
    );
    assert!(
        normalized.contains("/skills [list|detail] [name]"),
        "{output}"
    );
    // /mcp renders as an indented Registry-group entry with its own summary
    // line, like /extensions and /skills; Registry-group membership is pinned
    // by the registry unit tests.
    assert!(normalized.contains("│   /mcp"), "{output}");
    assert!(
        normalized.contains("│       manage MCP servers"),
        "{output}"
    );
    assert!(normalized.contains("/agent"), "{output}");
    assert!(!output.contains("/explain"), "{output}");
    assert!(!output.contains("/cancel"), "{output}");
    assert!(!output.contains("/details"), "{output}");
    assert!(!output.contains("command_id"), "{output}");
    assert!(!output.contains("output_id"), "{output}");
    assert!(!output.contains("insight_id"), "{output}");
    assert!(!output.contains("/audit"), "{output}");
    assert!(!output.contains("/select N"), "{output}");
    assert!(!output.contains("/copy N"), "{output}");
    assert!(!output.contains("/mode [recommend|auto|trust]"), "{output}");
    assert!(
        !output.contains("/approval-mode [suggest|ask|auto|trust]"),
        "{output}"
    );
    assert!(!output.contains("advanced legacy governance"), "{output}");
    assert!(!output.contains("/allow <n>"), "{output}");
    assert!(!output.contains("[ask|auto]alias"), "{output}");
    assert!(!output.contains("cosh-osc$ ╭ Slash commands"), "{output}");
    assert!(output.contains("Mode: auto."), "{output}");
    assert!(output.contains("after-help"), "{output}");
    assert!(!output.contains("bash: /help"), "{output}");
}

#[test]
fn raw_cli_status_about_and_stats_render_without_reaching_bash() {
    let output = run_raw_cli_with_env(
        "fake",
        "/status\n\
         /about\n\
         /stats\n\
         /stats model\n\
         /stats tools\n\
         echo after-status-queries\n\
         exit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    assert!(output.contains("cosh-shell:"), "{output}");
    assert!(output.contains("Backend: fake"), "{output}");
    assert!(output.contains("Provider: fake (test)"), "{output}");
    assert!(output.contains("Model: fake"), "{output}");
    assert!(output.contains("Session stats"), "{output}");
    assert!(output.contains("Model stats"), "{output}");
    assert!(output.contains("Tool stats"), "{output}");
    assert!(
        output.contains("No tool calls have been recorded in this session."),
        "{output}"
    );
    assert!(output.contains("after-status-queries"), "{output}");
    for command in ["/status", "/about", "/stats"] {
        assert!(
            !output.contains(&format!("bash: {command}:")),
            "{command} reached bash: {output}"
        );
        assert!(
            !output.contains(&format!("bash: {command}: No such file or directory")),
            "{command} reached bash: {output}"
        );
    }
}

#[test]
fn raw_cli_zsh_status_about_and_stats_render_without_reaching_zsh() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/status\n".to_vec(), Duration::ZERO),
            (b"/about\n".to_vec(), Duration::from_millis(100)),
            (b"/stats tools\n".to_vec(), Duration::from_millis(100)),
            (
                b"echo after-zsh-status-queries\n".to_vec(),
                Duration::from_millis(100),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Backend: fake"), "{output}");
    assert!(output.contains("Tool stats"), "{output}");
    assert!(output.contains("after-zsh-status-queries"), "{output}");
    for command in ["/status", "/about", "/stats"] {
        assert!(
            !output.contains(&format!("zsh: no such file or directory: {command}")),
            "{command} reached zsh: {output}"
        );
    }
}

#[test]
fn raw_cli_unknown_slash_suggests_nearest_canonical_command() {
    let output = run_raw_cli_with_input("fake", "/hep\necho after-unknown\nexit\n");

    assert!(output.contains("Unknown slash command: /hep"), "{output}");
    assert!(output.contains("Did you mean /help?"), "{output}");
    assert!(!output.contains("/approval-mode"), "{output}");
    assert!(output.contains("after-unknown"), "{output}");
    assert!(!output.contains("bash: /hep"), "{output}");
}

#[test]
fn raw_cli_unknown_slash_uses_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/hep\n\
         echo after-unknown-zh\n\
         exit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("未知 slash 命令: /hep"), "{output}");
    assert!(output.contains("你是不是想用 /help？"), "{output}");
    assert!(output.contains("使用 /help 查看可用命令。"), "{output}");
    assert!(!output.contains("Unknown slash command"), "{output}");
    assert!(!output.contains("Did you mean /help?"), "{output}");
    assert!(
        !output.contains("Use /help to see available commands."),
        "{output}"
    );
    assert!(output.contains("after-unknown-zh"), "{output}");
    assert!(!output.contains("bash: /hep"), "{output}");
    assert_no_migrated_english_ui_labels(&output, SLASH_CONFIG_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_informational_slash_commands_render_feedback() {
    let output = run_raw_cli_with_input(
        "fake",
        "/extensions\n\
         /config\n\
         echo after-info-slash\n\
         exit\n",
    );

    // /extensions with fake adapter shows degradation message
    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "{output}"
    );
    assert!(output.contains("Config"), "{output}");
    assert!(output.contains("language:"), "{output}");
    assert!(output.contains("debug activity: off"), "{output}");
    assert!(output.contains("Use /config language"), "{output}");
    assert!(output.contains("after-info-slash"), "{output}");
    assert!(!output.contains("bash: /skill"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
    assert!(!output.contains("bash: /audit"), "{output}");
}

#[test]
fn raw_cli_bare_slash_is_noop_without_hint_card() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"/\n".to_vec(), Duration::ZERO),
            (
                b"echo after-bare-slash\n".to_vec(),
                Duration::from_millis(200),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(!output.contains("Slash command hint"), "{output}");
    assert!(!output.contains("/help  /mode"), "{output}");
    assert!(!output.contains("bash: /"), "{output}");
    assert!(output.contains("after-bare-slash"), "{output}");
}

#[test]
fn raw_cli_slash_prefix_renders_hint_without_leaking_to_shell() {
    let output = run_raw_cli_with_input(
        "fake",
        "/mo\n\
         echo after-slash-hint\n\
         exit\n",
    );

    assert!(output.contains("Slash command hint"), "{output}");
    assert!(
        output.contains("/mode approval [recommend|auto|trust] - change approval mode"),
        "{output}"
    );
    assert!(!output.contains("/allow <n>"), "{output}");
    assert!(output.contains("Prefix: /mo"), "{output}");
    assert!(output.contains("after-slash-hint"), "{output}");
    assert!(!output.contains("cosh-osc$ ╭ Slash command"), "{output}");
    assert!(!output.contains("bash: /:"), "{output}");
    assert!(!output.contains("bash: /mo"), "{output}");
}

#[test]
fn raw_cli_slash_cards_wrap_long_text_and_restore_prompt() {
    let output = run_raw_cli_with_env(
        "fake",
        "/help\n\
         echo after-long-slash\n\
         exit\n",
        &[("TERM", "xterm-256color"), ("COSH_SHELL_WIDTH", "72")],
    );

    assert!(output.contains("Slash commands"), "{output}");
    let normalized = strip_ansi_escape(&output);
    assert!(
        normalized.contains("/mode approval [recommend|auto|trust]"),
        "{output}"
    );
    assert!(normalized.contains("change approval mode"), "{output}");
    assert!(output.contains("after-long-slash"), "{output}");
    assert_agent_block_width(&output, 72);
    assert!(!output.contains("[ask|auto]alias"), "{output}");
    assert!(!output.contains("cosh-osc$ ╭ Slash"), "{output}");
    assert!(!output.contains("bash: /asdf"), "{output}");
}

#[test]
fn raw_cli_zsh_shell_arg_intercepts_fragmented_slash() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/he".to_vec(), Duration::ZERO),
            (b"lp\n".to_vec(), Duration::from_millis(50)),
            (
                b"echo after-zsh-slash\n".to_vec(),
                Duration::from_millis(100),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Slash commands"), "{output}");
    assert!(
        strip_ansi_escape(&output).contains("/mode approval [recommend|auto|trust]"),
        "{output}"
    );
    assert!(output.contains("after-zsh-slash"), "{output}");
    assert!(
        !output.contains("zsh: no such file or directory: /help"),
        "{output}"
    );
}

#[test]
fn raw_cli_mcp_slash_is_intercepted() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mcp\n/mcp help\n/mcp list\necho after-mcp\nexit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    // /mcp should be intercepted, not passed to bash
    assert!(
        !output.contains("bash: /mcp: No such file or directory"),
        "/mcp reached bash: {output}"
    );
    assert!(
        !output.contains("bash: /mcp:"),
        "/mcp reached bash: {output}"
    );
    // /mcp help should show usage (handled in slash command, not backend)
    assert!(output.contains("Usage: /mcp"), "{output}");
    assert!(output.contains("list"), "{output}");
    assert!(output.contains("connect"), "{output}");
    assert!(output.contains("inspect"), "{output}");
    // /mcp list with fake adapter shows backend requirement
    assert!(
        output.contains("This feature requires cosh-core backend"),
        "/mcp should show backend requirement: {output}"
    );
    assert!(output.contains("after-mcp"), "{output}");
}

#[test]
fn raw_cli_mcp_unknown_subcommand_shows_error() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mcp unknown\necho after-mcp-unknown\nexit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    assert!(
        output.contains("Unknown subcommand: unknown"),
        "should show unknown subcommand error: {output}"
    );
    assert!(
        output.contains("Run /mcp help for usage information"),
        "should suggest help: {output}"
    );
    assert!(output.contains("after-mcp-unknown"), "{output}");
}

#[test]
fn raw_cli_mcp_missing_server_argument_shows_error() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mcp connect\n/mcp inspect\n/mcp refresh\necho after-mcp-no-server\nexit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    // Commands requiring server argument should show error when missing
    assert!(
        output.contains("error:") || output.contains("Error"),
        "should show error for missing server: {output}"
    );
    assert!(output.contains("after-mcp-no-server"), "{output}");
}

#[test]
fn raw_cli_mcp_login_redirects_to_shell() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mcp login myserver\necho after-mcp-login\nexit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    // OAuth login cannot complete inside the synchronous TUI path; the
    // command must short-circuit with shell guidance instead of spawning
    // a subprocess that would time out.
    assert!(
        output.contains("cannot run inside the TUI"),
        "login should be intercepted before the backend: {output}"
    );
    assert!(
        output.contains("cosh-core mcp login myserver"),
        "guidance should name the requested server: {output}"
    );
    assert!(
        !output.contains("This feature requires cosh-core backend"),
        "login guidance must not depend on the backend: {output}"
    );
    assert!(output.contains("after-mcp-login"), "{output}");
}

#[test]
fn raw_cli_mcp_extra_argument_shows_error() {
    let output = run_raw_cli_with_env(
        "fake",
        "/mcp connect one two\necho after-mcp-extra\nexit\n",
        &[("COSH_SHELL_LANG", "en-US")],
    );

    assert!(
        output.contains("unexpected argument: two"),
        "trailing arguments must be rejected, not silently ignored: {output}"
    );
    assert!(output.contains("after-mcp-extra"), "{output}");
}

#[test]
fn raw_cli_zsh_mcp_slash_is_intercepted() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "zsh"],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"/mcp\n".to_vec(), Duration::ZERO),
            (b"/mcp help\n".to_vec(), Duration::from_millis(100)),
            (b"echo after-zsh-mcp\n".to_vec(), Duration::from_millis(100)),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(
        !output.contains("zsh: no such file or directory: /mcp"),
        "/mcp reached zsh: {output}"
    );
    assert!(output.contains("Usage: /mcp"), "{output}");
    assert!(output.contains("after-zsh-mcp"), "{output}");
}
