use super::*;

#[test]
fn raw_cli_invalid_shell_integration_fails_visibly() {
    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let output = raw_cli_command(binary)
        .args(["raw", "fake"])
        .env("COSH_SHELL_INTEGRATION", "enhacned")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run invalid Shell integration");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("invalid shell integration") && stderr.contains("native or enhanced"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn raw_cli_config_language_save_applies_to_current_session_help() {
    let home = temp_shell_home("config-language-current-session");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("HOME", &home_str)],
        vec![
            (b"/config language zh-CN\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            (b"/help\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("配置已保存"), "{output}");
    assert!(output.contains("当前会话语言: zh-CN。"), "{output}");
    assert!(output.contains("Slash 命令"), "{output}");
    assert!(output.contains("配置"), "{output}");
    assert!(!output.contains("Config saved"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
    assert!(!output.contains("bash: /help"), "{output}");
    assert_no_migrated_english_ui_labels(&output, SLASH_CONFIG_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_config_summary_reads_language_from_user_config() {
    let home = temp_shell_home("config-language-summary");
    write_cosh_config(
        &home,
        r#"
[ui]
language = "zh-CN"
debug = true
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_env(
        "fake",
        "/config\nexit\n",
        &[("HOME", &home_str), ("COSH_SHELL_LANG", RAW_CLI_UNSET_ENV)],
    );

    assert!(output.contains("配置"), "{output}");
    assert!(output.contains("语言: zh-CN 来源: config"), "{output}");
    assert!(output.contains("调试活动: on"), "{output}");
    assert!(output.contains("config.toml"), "{output}");
    assert!(
        output.contains("使用 /config language [auto|en-US|zh-CN]"),
        "{output}"
    );
    assert!(!output.contains("bash: /config"), "{output}");
}

#[test]
fn raw_cli_config_summary_ignores_legacy_user_config() {
    let home = temp_shell_home("config-language-legacy-ignored");
    write_legacy_cosh_config(
        &home,
        r#"
[ui]
language = "zh-CN"
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_env(
        "fake",
        "/config\nexit\n",
        &[("HOME", &home_str), ("COSH_SHELL_LANG", RAW_CLI_UNSET_ENV)],
    );

    assert!(output.contains("Config"), "{output}");
    assert!(output.contains("language: en-US effective"), "{output}");
    assert!(!output.contains("语言: zh-CN 来源: config"), "{output}");
    assert!(output.contains(".copilot-shell"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
}

#[test]
fn raw_cli_config_language_errors_use_zh_language_env() {
    let output = run_raw_cli_with_env(
        "fake",
        "/config language nope\n/config unknown\nexit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("配置"), "{output}");
    assert!(output.contains("无效语言: nope"), "{output}");
    assert!(output.contains("支持: auto, en-US, zh-CN。"), "{output}");
    assert!(output.contains("未知配置项: unknown"), "{output}");
    assert!(!output.contains("Invalid language"), "{output}");
    assert!(!output.contains("Unknown config key"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
    assert_no_migrated_english_ui_labels(&output, SLASH_CONFIG_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_config_language_direct_set_saves_after_confirmation() {
    let home = temp_shell_home("config-language-save");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("COSH_SHELL_LANG", RAW_CLI_UNSET_ENV)],
        vec![
            (b"/config language zh-CN\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            (b"/config\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    let config_path = home.join(".copilot-shell/config.toml");
    let content = fs::read_to_string(&config_path).expect("read saved config");
    assert!(content.contains("[ui]"), "{content}");
    assert!(content.contains("language = \"zh-CN\""), "{content}");
    assert!(output.contains("Save config?"), "{output}");
    assert!(output.contains("配置已保存"), "{output}");
    assert!(
        output.contains("保存的设置立即生效；Agent 回复跟随你的提问语言。"),
        "{output}"
    );
    assert!(output.contains("语言: zh-CN 来源: config"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
}

#[test]
fn raw_cli_config_language_selector_saves_after_confirmation() {
    let home = temp_shell_home("config-language-selector-save");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("COSH_SHELL_LANG", RAW_CLI_UNSET_ENV)],
        vec![
            (b"/config language\n".to_vec(), Duration::ZERO),
            (b"\x1b[C\x1b[C\n".to_vec(), Duration::from_millis(1_200)),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            (b"/config\n".to_vec(), Duration::from_millis(200)),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    let config_path = home.join(".copilot-shell/config.toml");
    let content = fs::read_to_string(&config_path).expect("read saved config");
    assert!(content.contains("language = \"zh-CN\""), "{content}");
    assert!(output.contains("Language"), "{output}");
    assert!(output.contains("zh-CN"), "{output}");
    assert!(output.contains("Save config?"), "{output}");
    assert!(output.contains("配置已保存"), "{output}");
    assert!(output.contains("语言: zh-CN 来源: config"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
    assert!(!output.contains("^[[C"), "{output}");
}

#[test]
fn raw_cli_config_language_selector_cancel_does_not_write_file() {
    let home = temp_shell_home("config-language-selector-cancel");
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("HOME", &home_str)],
        vec![
            (b"/config language\n".to_vec(), Duration::ZERO),
            (b"\x1b\n".to_vec(), Duration::from_millis(1_200)),
            (
                b"echo after-config-cancel\n".to_vec(),
                Duration::from_millis(200),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(!home.join(".copilot-shell/config.toml").exists());
    assert!(output.contains("Language"), "{output}");
    assert!(output.contains("Config unchanged"), "{output}");
    assert!(output.contains("No config file was changed."), "{output}");
    assert!(output.contains("after-config-cancel"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
}

#[test]
fn raw_cli_config_env_language_override_does_not_rewrite_file() {
    let home = temp_shell_home("config-language-env");
    write_cosh_config(
        &home,
        r#"
[ui]
language = "zh-CN"
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_env(
        "fake",
        "/config\nexit\n",
        &[("HOME", &home_str), ("COSH_SHELL_LANG", "en-US")],
    );

    let config_path = home.join(".copilot-shell/config.toml");
    let content = fs::read_to_string(&config_path).expect("read saved config");
    assert!(content.contains("language = \"zh-CN\""), "{content}");
    assert!(output.contains("language: en-US source: env"), "{output}");
    assert!(!output.contains("bash: /config"), "{output}");
}
