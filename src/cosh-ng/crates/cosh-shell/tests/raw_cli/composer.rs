use super::*;

fn run_composer_slash_steps(steps: &[(&str, &[u8])]) -> String {
    let home = tempfile::Builder::new()
        .prefix("composer-slash-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        .unwrap();
    fs::write(home.path().join(".bashrc"), "PS1='composer-test$ '\n").unwrap();
    let home_str = home.path().to_string_lossy().to_string();
    let mut input = vec![("composer-test$", b"/agent\n".as_slice())];
    input.extend_from_slice(steps);
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        home.path(),
        &input,
    );
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
    assert!(!output.contains("Sent to agent"), "{output}");
    output
}

#[test]
fn raw_cli_agent_composer_lists_completes_and_executes_slash_commands() {
    let output = run_composer_slash_steps(&[
        ("Agent Composer", b"/"),
        ("› /help", b"ho\t\r"),
        ("Hook status", b"echo after-slash\n"),
        ("after-slash", b"exit\n"),
    ]);
    assert!(output.contains("/hooks"), "{output}");
    assert!(output.contains("Command submitted"), "{output}");
    assert!(output.contains("after-slash"), "{output}");
    assert!(!output.contains("bash: /hooks"), "{output}");
}

#[test]
fn raw_cli_agent_composer_enter_executes_the_selected_command() {
    let select_hooks = b"\x1b[B".repeat(11);
    let output = run_composer_slash_steps(&[
        ("Agent Composer", b"/"),
        ("› /help", &select_hooks),
        ("› /hooks", b"\r"),
        ("Hook status", b"echo after-enter\n"),
        ("after-enter", b"exit\n"),
    ]);
    assert!(output.contains("◆ /hooks"), "{output}");
    assert!(output.contains("after-enter"), "{output}");
}

#[test]
fn raw_cli_agent_composer_slash_completion_scrolls_to_the_last_command() {
    let mut keys = b"/".to_vec();
    for _ in 0..32 {
        keys.extend_from_slice(b"\x1b[B");
    }
    let output = run_composer_slash_steps(&[
        ("Agent Composer", &keys),
        ("› /mcp", b"\x1b"),
        ("Draft cancelled", b"exit\n"),
    ]);
    assert!(output.contains("› /mcp"), "{output}");
}

#[test]
fn raw_cli_agent_composer_control_command_hands_input_to_its_card() {
    let output = run_composer_slash_steps(&[
        ("Agent Composer", b"/mode approval\r"),
        ("User mode", b"\x1b"),
        ("Mode unchanged:", b"echo after-mode\n"),
        ("after-mode", b"exit\n"),
    ]);
    assert!(output.contains("after-mode"), "{output}");
    assert!(!output.contains("bash: /mode"), "{output}");
}

#[test]
fn raw_cli_agent_composer_unknown_command_is_local() {
    let output = run_composer_slash_steps(&[
        ("Agent Composer", b"/not-a-command\r"),
        ("Unknown slash command:", b"echo after-unknown\n"),
        ("after-unknown", b"exit\n"),
    ]);
    assert!(output.contains("after-unknown"), "{output}");
    assert!(!output.contains("bash: /not-a-command"), "{output}");
}

#[test]
fn raw_cli_agent_composer_hidden_controls_restore_prompt_after_their_output() {
    for (command, notice) in [
        (b"/cancel\r".as_slice(), "no active Agent run"),
        (b"/details missing\r".as_slice(), "Details unavailable"),
    ] {
        let output = run_composer_slash_steps(&[
            ("Agent Composer", command),
            (notice, b""),
            ("composer-test$", b"echo after-control\n"),
            ("after-control", b"exit\n"),
        ]);
        assert!(
            output.rfind("◆ ").unwrap() < output.find(notice).unwrap(),
            "{output}"
        );
    }
}

#[test]
fn raw_cli_agent_composer_health_uses_the_current_shell_workspace() {
    let home = tempfile::Builder::new()
        .prefix("composer-health-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        .unwrap();
    fs::write(home.path().join(".bashrc"), "PS1='health-test$ '\n").unwrap();
    let hooks = home.path().join("project/.cosh/hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    let home_str = home.path().to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        home.path(),
        &[
            ("health-test$", b"cd project\n"),
            ("health-test$", b"/agent\n"),
            ("Agent Composer", b"/health\r"),
            ("review and trust project hooks under", b"exit\n"),
        ],
    );
    assert!(output.contains("Health check"), "{output}");
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
}

#[test]
fn raw_cli_composer_change_preserves_bash_path_completion() {
    let home = tempfile::Builder::new()
        .prefix("composer-native-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        .unwrap();
    fs::write(home.path().join(".bashrc"), "PS1='path-test$ '\n").unwrap();
    fs::create_dir(home.path().join("path-target")).unwrap();
    let home_str = home.path().to_string_lossy().to_string();
    let completion = format!("{home_str}/path-ta\t");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        home.path(),
        &[
            ("path-test$", completion.as_bytes()),
            ("path-target/", b"\x15echo native-completion-ok\n"),
            ("native-completion-ok", b"exit\n"),
        ],
    );
    assert!(!output.contains("Agent Composer"), "{output}");
    assert!(
        !output.contains("Received shell prompt request:"),
        "{output}"
    );
}

#[test]
fn raw_cli_removed_draft_alias_falls_through_to_shell() {
    let home = temp_shell_home("agent-composer-removed-draft-alias");
    fs::write(home.join(".bashrc"), "PS1='alias-test$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("alias-test$", b"/draft\n"),
            ("No such file or directory", b"exit 0\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("bash: /draft"), "{output}");
    assert!(!output.contains("Agent Composer"), "{output}");
}

#[test]
fn raw_cli_bash_agent_composer_submits_multiline_request_and_restores_custom_prompt() {
    let home = temp_shell_home("agent-composer-bash");
    fs::write(home.join(".bashrc"), "PS1='alice@remote:\\w$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        cwd,
        &[
            ("alice@remote:", b"/agent\n"),
            (
                "Agent Composer",
                b"/skill:repo-review inspect @Cargo.toml\x1b[13;2uand @src\r",
            ),
            ("Received shell prompt request:", b"echo after-composer\n"),
            ("after-composer", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Runtime: fake"), "{output}");
    assert!(
        output.contains("◆ "),
        "Agent must own composer input: {output}"
    );
    assert!(!output.contains("╭ Agent Composer"), "{output}");
    assert!(
        output.contains("/skill:repo-review inspect @Cargo.toml"),
        "{output}"
    );
    assert!(output.contains("and @src"), "{output}");
    assert!(output.contains("after-composer"), "{output}");
    assert!(count_occurrences(&output, "alice@remote:") >= 2, "{output}");
    let visible = strip_ansi_escape(&output).replace('\r', "");
    assert!(
        count_occurrences(&visible, "\nalice@remote:") >= 2,
        "Enhanced must render both the initial and restored Shell prompt: {output}"
    );
    assert!(
        !visible.contains("◇ ") && !visible.contains("◌ "),
        "no status symbol lines may be emitted: {output}"
    );
    assert!(!output.contains("bash: /agent"), "{output}");
    let composer = output.find("Agent Composer").expect("composer card");
    let draft_text = output[composer..]
        .find("/skill:repo-review")
        .map(|offset| composer + offset)
        .expect("composer input");
    assert!(
        !strip_ansi_escape(&output[composer..draft_text]).contains("alice@remote:"),
        "the shell prompt must stay hidden while the composer owns input: {output}"
    );
}

#[test]
fn raw_cli_agent_composer_suggests_and_accepts_workspace_paths() {
    let home = temp_shell_home("agent-composer-path-completion");
    fs::write(home.join(".bashrc"), "PS1='alice@remote:\\w$ '\n").unwrap();
    let home_str = home.to_string_lossy().to_string();
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "bash"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        cwd,
        &[
            ("alice@remote:", b"/agent\n"),
            ("Agent Composer", b"review @Car"),
            ("› @Cargo.toml", b"\tinspect\x1b"),
            ("Draft cancelled", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("› @Cargo.toml"), "{output}");
    assert!(output.contains("review @Cargo.toml inspect"), "{output}");
}

#[test]
fn raw_cli_zsh_agent_composer_cancel_restores_custom_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let home = temp_zsh_home("agent-composer-zsh");
    fs::write(
        home.join(".zshrc"),
        "PROMPT='zsh@remote:%~%# '\nRPROMPT=''\n",
    )
    .unwrap();
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &["--shell", "zsh"],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("zsh@remote:", b"/agent\n"),
            ("Agent Composer", b"cancel this draft\x1b"),
            ("Draft cancelled", b"echo after-cancel\n"),
            ("after-cancel", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Runtime: fake"), "{output}");
    assert!(output.contains("Draft cancelled"), "{output}");
    assert!(output.contains("after-cancel"), "{output}");
    assert!(count_occurrences(&output, "zsh@remote:") >= 2, "{output}");
    let visible = strip_ansi_escape(&output).replace('\r', "");
    assert!(
        count_occurrences(&visible, "\nzsh@remote:") >= 2,
        "Enhanced must render both the initial and restored Zsh prompt: {output}"
    );
    assert!(
        !visible.contains("◇ ") && !visible.contains("◌ "),
        "no status symbol lines may be emitted: {output}"
    );
    assert!(
        !output.contains("Received shell prompt request: cancel this draft"),
        "{output}"
    );
}
