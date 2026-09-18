use super::*;

fn bash_available() -> bool {
    match Command::new("bash").arg("--version").output() {
        Ok(_) => true,
        Err(error) => {
            eprintln!("SKIP: bash is unavailable: {error}");
            false
        }
    }
}

fn secondary_prompt_spans(terminal: &str, expected_ps2: &str) -> Result<[usize; 4], String> {
    const PS1: &str = "__USER_PS1__ ";
    const MULTILINE_OUTPUT: &str = "__MULTILINE__<alpha\nbeta>";

    let ps1 = terminal
        .match_indices(PS1)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if ps1.len() != 2 {
        return Err(format!("expected exactly two PS1 spans, got {ps1:?}"));
    }
    let ps2 = terminal
        .match_indices(expected_ps2)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if ps2.len() != 1 {
        return Err(format!("expected exactly one PS2 span, got {ps2:?}"));
    }
    let output = terminal[ps2[0] + expected_ps2.len()..]
        .find(MULTILINE_OUTPUT)
        .map(|offset| ps2[0] + expected_ps2.len() + offset)
        .ok_or_else(|| "missing exact multiline output after PS2".to_string())?;
    let spans = [ps1[0], ps2[0], output, ps1[1]];
    if !spans.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(format!("prompt spans are out of order: {spans:?}"));
    }
    let ps2_line_start = terminal[..ps2[0]].rfind('\n').map_or(0, |index| index + 1);
    if terminal[ps2_line_start..ps2[0]].contains('◇') {
        return Err("PS2 line contains a primary-prompt owner".to_string());
    }
    Ok(spans)
}

#[test]
fn secondary_prompt_oracle_rejects_wrong_order_and_duplicates() {
    let wrong_order = concat!(
        "__ENV_PS2__ ",
        "__MULTILINE__<alpha\nbeta>\n",
        "__USER_PS1__ __USER_PS1__ "
    );
    assert!(secondary_prompt_spans(wrong_order, "__ENV_PS2__ ").is_err());

    let duplicate_ps2 = concat!(
        "__USER_PS1__ \n__ENV_PS2__ \n__ENV_PS2__ ",
        "__MULTILINE__<alpha\nbeta>\n__USER_PS1__ "
    );
    assert!(secondary_prompt_spans(duplicate_ps2, "__ENV_PS2__ ").is_err());
}

#[test]
fn bash_secondary_prompt_stays_shell_owned() {
    if !bash_available() {
        return;
    }

    for (integration, login_shell, environment_ps2, startup_ps2) in
        [false, true].into_iter().flat_map(|login_shell| {
            [
                (
                    ShellIntegration::Enhanced,
                    login_shell,
                    Some("__ENV_PS2__ "),
                    None,
                ),
                (
                    ShellIntegration::Enhanced,
                    login_shell,
                    Some("__ENV_PS2__ "),
                    Some("__STARTUP_PS2__ "),
                ),
                (
                    ShellIntegration::Native,
                    login_shell,
                    None,
                    Some("__NATIVE_PS2__ "),
                ),
            ]
        })
    {
        let mode = if login_shell { "login" } else { "nonlogin" };
        let integration_name = if integration == ShellIntegration::Enhanced {
            "enhanced"
        } else {
            "native"
        };
        let root = tempfile::Builder::new()
            .prefix(&format!("cosh-shell-ps2-{integration_name}-{mode}-"))
            .tempdir()
            .expect("temporary PS2 root");
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let startup = if login_shell {
            home.join(".bash_profile")
        } else {
            home.join(".bashrc")
        };
        let mut startup_contents = "PS1='__USER_PS1__ '\n".to_string();
        if let Some(ps2) = startup_ps2 {
            startup_contents.push_str(&format!("PS2='{ps2}'\n"));
        }
        std::fs::write(startup, startup_contents).expect("startup file");

        let mut config = ShellHostConfig::new("ps2-parity", root.path().join("work"))
            .with_integration(integration)
            .with_env("HOME", home.display().to_string());
        if let Some(ps2) = environment_ps2 {
            config = config.with_env("PS2", ps2);
        }
        config.login_shell = login_shell;
        config.raw_action_watchdog = Duration::from_secs(2);
        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_actions(
            &config,
            vec![
                RawRelayAction::wait(Duration::from_millis(200)),
                RawRelayAction::line("printf '__MULTILINE__<%s>\\n' 'alpha"),
                RawRelayAction::wait(Duration::from_millis(200)),
                RawRelayAction::line("beta'"),
                RawRelayAction::wait(Duration::from_millis(200)),
                RawRelayAction::line("exit"),
            ],
            &mut rendered,
        )
        .unwrap_or_else(|error| panic!("{integration_name}/{mode}: {error}"));
        let terminal = without_readline_mode_controls(&String::from_utf8_lossy(&rendered))
            .replace("\r\n", "\n");
        let expected_ps2 = startup_ps2.or(environment_ps2).expect("PS2 fixture");

        secondary_prompt_spans(&terminal, expected_ps2)
            .unwrap_or_else(|error| panic!("{integration_name}/{mode}: {error}: {terminal:?}"));
        if let (Some(environment_ps2), Some(_)) = (environment_ps2, startup_ps2) {
            assert!(!terminal.contains(environment_ps2), "{terminal:?}");
        }
        if integration == ShellIntegration::Native {
            assert!(!terminal.contains('◇'), "{terminal:?}");
        }
        assert_eq!(output.exit_status, Some(0), "{terminal:?}");
    }
}

#[test]
fn bash_secondary_prompt_watchdog_reaps_unclosed_continuation() {
    if !bash_available() {
        return;
    }

    let root = tempfile::Builder::new()
        .prefix("cosh-shell-ps2-watchdog-")
        .tempdir()
        .expect("temporary PS2 watchdog root");
    let mut config = ShellHostConfig::new("ps2-watchdog", root.path().join("work"))
        .with_integration(ShellIntegration::Native);
    config.raw_action_watchdog = Duration::from_millis(100);
    let error = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("printf 'unterminated"),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
    )
    .expect_err("unclosed continuation must hit the action watchdog");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
}

#[test]
fn enhanced_bash_errexit_context_keeps_interactive_session_alive() {
    if !bash_available() {
        return;
    }

    for login_shell in [false, true] {
        let mode = if login_shell { "login" } else { "nonlogin" };
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-errexit-{mode}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let mut config = ShellHostConfig::new(format!("errexit-{mode}"), &work_dir)
            .with_integration(ShellIntegration::Enhanced)
            .with_env("LANG", "C.UTF-8")
            .with_env("LC_ALL", "C.UTF-8");
        config.login_shell = login_shell;

        let output = run_scripted_bash(
            &config,
            &[
                ScriptedInput::command(
                    "{ set -e; false && printf bad; printf '__ERREXIT_CONTEXT__\\n'; }",
                ),
                ScriptedInput::command("printf '__SESSION_CONTINUED__\\n'"),
            ],
        )
        .unwrap_or_else(|error| panic!("{mode} scripted bash: {error}"));

        let terminal = String::from_utf8_lossy(&output.terminal_output);
        assert!(
            terminal.contains("__ERREXIT_CONTEXT__"),
            "{mode}: {terminal}"
        );
        assert!(
            terminal.contains("__SESSION_CONTINUED__"),
            "{mode}: {terminal}"
        );
        assert_eq!(output.exit_status, Some(0), "{mode}: {terminal}");

        let context = output
            .events
            .iter()
            .find(|event| {
                event.kind == ShellEventKind::CommandCompleted
                    && event.command.as_deref().is_some_and(|command| {
                        command.contains("false && printf bad")
                            && command.contains("__ERREXIT_CONTEXT__")
                    })
            })
            .unwrap_or_else(|| panic!("{mode}: missing errexit completion: {:?}", output.events));
        assert_eq!(context.exit_code, Some(0), "{mode}: {terminal}");
        assert!(
            output.events.iter().any(|event| {
                event.kind == ShellEventKind::CommandCompleted
                    && event.command.as_deref() == Some("printf '__SESSION_CONTINUED__\\n'")
                    && event.exit_code == Some(0)
            }),
            "{mode}: missing continuation completion: {:?}",
            output.events
        );

        let _ = std::fs::remove_dir_all(&work_dir);
    }
}

#[test]
fn enhanced_bash_preserves_last_argument_across_prompt_boundary() {
    if !bash_available() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-last-argument-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("last-argument", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("LANG", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8");
    let output = run_scripted_bash(
        &config,
        &[
            ScriptedInput::command("echo hello world"),
            ScriptedInput::command("printf '__LAST_ARGUMENT__=[%s]\\n' \"$_\""),
        ],
    )
    .expect("scripted bash");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("__LAST_ARGUMENT__=[world]"), "{terminal}");

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn bash_prompt_command_attributes_and_child_environment_follow_integration_contract() {
    if !bash_available() {
        return;
    }

    let supports_array = bash_supports_prompt_command_array();
    for (fixture, inherited, startup, expected_trace) in [
        ("scalar", "USER_PROMPT_TRACE+=scalar", "", "scalar"),
        (
            "array",
            "",
            "PROMPT_COMMAND=('USER_PROMPT_TRACE+=first' 'USER_PROMPT_TRACE+=second')\n",
            if supports_array {
                "firstsecond"
            } else {
                "first"
            },
        ),
        ("empty", "", "", ""),
        (
            "unassigned",
            "",
            "unset PROMPT_COMMAND; export PROMPT_COMMAND\n",
            "",
        ),
    ] {
        for login_shell in [false, true] {
            for integration in [ShellIntegration::Native, ShellIntegration::Enhanced] {
                let enhanced = integration == ShellIntegration::Enhanced;
                let context = format!("{fixture}/login={login_shell}/{integration:?}");
                let root = tempfile::Builder::new()
                    .prefix("cosh-shell-prompt-command-")
                    .tempdir()
                    .expect("prompt command root");
                let home = root.path().join("home");
                std::fs::create_dir_all(&home).expect("home");
                let startup_file = if login_shell {
                    ".bash_profile"
                } else {
                    ".bashrc"
                };
                // System login profiles may rewrite PROMPT_COMMAND before this
                // user startup file. Establish each fixture after those hooks.
                std::fs::write(
                    home.join(startup_file),
                    format!(
                        "PS1='__PROMPT_COMMAND_READY__ '\nUSER_PROMPT_TRACE=\n\
                         unset PROMPT_COMMAND\nexport PROMPT_COMMAND='{inherited}'\n{startup}"
                    ),
                )
                .expect("startup file");
                let mut config =
                    ShellHostConfig::new("prompt-command-env", root.path().join("work"))
                        .with_integration(integration)
                        .with_env("HOME", home.display().to_string())
                        .with_env("PROMPT_COMMAND", inherited)
                        .with_env("LANG", "C.UTF-8")
                        .with_env("LC_ALL", "C.UTF-8");
                config.login_shell = login_shell;
                config.raw_action_watchdog = Duration::from_secs(2);
                let mut rendered = Vec::new();
                let output = run_raw_relay_bash_with_actions(
                    &config,
                    vec![
                        RawRelayAction::wait(Duration::from_millis(100)),
                        RawRelayAction::line(
                            "printf '%s' \"$USER_PROMPT_TRACE\" > \"$HOME/hook-trace\"; \
                             declare -p PROMPT_COMMAND > \"$HOME/declaration\"; \
                             env | grep '^PROMPT_COMMAND=' > \"$HOME/child-env\" || :; \
                             (PROMPT_COMMAND=('printf p1' 'printf p2'); \
                              declare -p PROMPT_COMMAND > \"$HOME/reassigned\")",
                        ),
                        RawRelayAction::line("exit"),
                    ],
                    &mut rendered,
                )
                .unwrap_or_else(|error| panic!("{context}: {error}"));
                let terminal = String::from_utf8_lossy(&rendered);
                assert_eq!(output.exit_status, Some(0), "{context}: {terminal}");
                let read = |name| {
                    std::fs::read_to_string(home.join(name))
                        .unwrap_or_else(|error| panic!("{context}/{name}: {error}: {terminal}"))
                };
                assert_eq!(read("hook-trace"), expected_trace, "{context}");

                let declaration = read("declaration");
                let attributes = if enhanced {
                    if supports_array {
                        "-ax"
                    } else {
                        "--"
                    }
                } else if fixture == "array" {
                    "-ax"
                } else {
                    "-x"
                };
                assert!(
                    declaration.starts_with(&format!("declare {attributes} PROMPT_COMMAND")),
                    "{context}: {declaration}"
                );
                assert_eq!(
                    declaration.contains("_COSH_PROMPT_STATUS"),
                    enhanced,
                    "{context}"
                );
                assert_eq!(
                    declaration.contains("_cosh_bounded_prompt_marker"),
                    enhanced,
                    "{context}"
                );

                // Native launches Bash without a marker rcfile. Exported scalars
                // retain their original child value; arrays have no environment entry.
                let child_env = read("child-env");
                let child_value = child_env
                    .lines()
                    .find_map(|line| line.strip_prefix("PROMPT_COMMAND="));
                let expected_child = if !enhanced && matches!(fixture, "scalar" | "empty") {
                    Some(inherited)
                } else {
                    None
                };
                assert_eq!(child_value, expected_child, "{context}: {child_env}");
                let reassigned_attributes = if enhanced && !supports_array {
                    "-a"
                } else {
                    "-ax"
                };
                assert!(
                    read("reassigned")
                        .starts_with(&format!("declare {reassigned_attributes} PROMPT_COMMAND=")),
                    "{context}: {}",
                    read("reassigned")
                );
                if !enhanced {
                    assert!(
                        !config.work_dir.join("cosh-marker.bash").exists(),
                        "{context}"
                    );
                }
            }
        }
    }
}

#[test]
fn enhanced_bash_keeps_three_background_jobs_concurrent() {
    if !bash_available() {
        return;
    }

    for login_shell in [false, true] {
        let mode = if login_shell { "login" } else { "nonlogin" };
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-background-parity-{mode}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let results = shell_arg(&work_dir.join("background-results"));
        let release = shell_arg(&work_dir.join("background-release"));
        let command = format!(
            "rm -f {results} {release}; \
             for i in 1 2 3; do \
               (sleep 0.3; while [[ ! -e {release} ]]; do sleep 0.05; done; \
                printf 'h%s\\n' \"$i\" >> {results}) & \
             done; \
             third=$!; pids=($(jobs -p)); jobs -r; \
             if [[ \"$third\" == \"${{pids[2]-}}\" ]]; then \
               printf '__LAST_IS_THIRD__=yes\\n'; \
             else \
               printf '__LAST_IS_THIRD__=no\\n'; \
             fi; \
             : > {release}; wait; sort {results}"
        );
        let mut config = ShellHostConfig::new(format!("background-parity-{mode}"), &work_dir)
            .with_integration(ShellIntegration::Enhanced)
            .with_env("LANG", "C.UTF-8")
            .with_env("LC_ALL", "C.UTF-8");
        config.login_shell = login_shell;
        let output = run_scripted_bash(&config, &[ScriptedInput::command(command.clone())])
            .unwrap_or_else(|error| panic!("{mode} scripted bash: {error}"));

        let ledger = ledger_from_output(&output);
        let block = ledger
            .blocks
            .iter()
            .find(|block| block.command == command)
            .unwrap_or_else(|| panic!("{mode}: missing background command: {:#?}", ledger.blocks));
        assert_eq!(block.exit_code, 0, "{mode}");
        let output_ref = block
            .output
            .terminal_output_ref
            .as_deref()
            .unwrap_or_else(|| panic!("{mode}: background output ref"));
        let command_output = std::fs::read_to_string(output_ref)
            .unwrap_or_else(|error| panic!("{mode}: background output: {error}"));

        for job in ["[1]", "[2]", "[3]"] {
            assert!(command_output.contains(job), "{mode}: {command_output}");
        }
        assert_eq!(
            command_output
                .lines()
                .filter(|line| line.contains("Running"))
                .count(),
            3,
            "{mode}: {command_output}"
        );
        assert!(
            command_output.contains("__LAST_IS_THIRD__=yes"),
            "{mode}: {command_output}"
        );
        let lines = command_output.lines().map(str::trim).collect::<Vec<_>>();
        for result in ["h1", "h2", "h3"] {
            assert!(lines.contains(&result), "{mode}: {command_output}");
        }

        let _ = std::fs::remove_dir_all(&work_dir);
    }
}
