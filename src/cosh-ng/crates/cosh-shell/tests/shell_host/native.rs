use super::*;

#[test]
fn bash_startup_files_reach_user_prompt_in_native_and_enhanced_sessions() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("SKIP: bash is unavailable");
        return;
    }

    for (case, login, first_profile, syntax_error, expected_trace) in [
        ("bashrc", false, 0, false, "rc"),
        ("bash-profile", true, 0, false, "profile:rc"),
        ("bash-login", true, 1, false, "login:rc"),
        ("profile", true, 2, false, "fallback:rc"),
        ("bashrc-error", false, 0, true, "rc"),
        ("profile-error", true, 0, true, "profile:rc"),
    ] {
        for integration in [ShellIntegration::Native, ShellIntegration::Enhanced] {
            let context = format!("{case}/{integration:?}");
            let root = tempfile::Builder::new()
                .prefix("cosh-shell-startup-readiness-")
                .tempdir()
                .expect("startup readiness root");
            let home = root.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut rc = "STARTUP_TRACE+=rc\nPS1='__STARTUP_READY__ '\n".to_string();
            if syntax_error && !login {
                rc.push_str("if ; then\n");
            }
            std::fs::write(home.join(".bashrc"), rc).expect("bashrc");
            for (index, (file, tag)) in [
                (".bash_profile", "profile"),
                (".bash_login", "login"),
                (".profile", "fallback"),
            ]
            .into_iter()
            .enumerate()
            .skip(first_profile)
            {
                let mut profile = format!("STARTUP_TRACE+={tag}:\nsource \"$HOME/.bashrc\"\n");
                if syntax_error && login && index == first_profile {
                    profile.push_str("if ; then\n");
                }
                std::fs::write(home.join(file), profile).expect("profile");
            }

            let mut config = ShellHostConfig::new("startup-readiness", root.path().join("work"))
                .with_integration(integration)
                .with_env("HOME", home.display().to_string())
                .with_env("STARTUP_TRACE", "")
                .with_env("LANG", "C")
                .with_env("LC_ALL", "C");
            config.login_shell = login;
            config.raw_action_watchdog = Duration::from_secs(2);
            let mut rendered = Vec::new();
            let output = run_raw_relay_bash_with_actions(
                &config,
                vec![
                    RawRelayAction::wait(Duration::from_millis(100)),
                    RawRelayAction::line(
                        "printf '%s' \"$STARTUP_TRACE\" > \"$HOME/startup-observed\"; \
                         printf '__READY_%s__=%s\\n' RESULT \"$STARTUP_TRACE\"",
                    ),
                    RawRelayAction::line("exit"),
                ],
                &mut rendered,
            )
            .unwrap_or_else(|error| panic!("{context}: {error}"));
            let terminal = without_readline_mode_controls(&String::from_utf8_lossy(&rendered));
            assert_eq!(output.exit_status, Some(0), "{context}: {terminal}");
            assert_eq!(
                std::fs::read_to_string(home.join("startup-observed"))
                    .unwrap_or_else(|error| panic!("{context}: {error}: {terminal}")),
                expected_trace,
                "{context}"
            );
            let prompts = terminal
                .match_indices("__STARTUP_READY__ ")
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let result = terminal
                .find(&format!("__READY_RESULT__={expected_trace}"))
                .unwrap_or_else(|| panic!("{context}: missing command output: {terminal}"));
            assert_eq!(prompts.len(), 2, "{context}: {terminal}");
            assert!(
                prompts[0] < result && result < prompts[1],
                "{context}: {terminal}"
            );
            assert_eq!(
                terminal.contains("syntax error"),
                syntax_error,
                "{context}: {terminal}"
            );
            if syntax_error {
                assert!(
                    terminal.find("syntax error").expect("syntax diagnostic") < prompts[0],
                    "{context}: {terminal}"
                );
            }
        }
    }
}

fn assert_debug_log_has_only_bounded_cosh_capture(debug_log: &str) {
    for line in debug_log
        .lines()
        .filter(|line| line.contains("_cosh") || line.contains("_COSH"))
    {
        let command = line
            .strip_prefix("DBG=[")
            .and_then(|line| line.strip_suffix(']'))
            .unwrap_or(line);
        assert!(
            command.starts_with("_COSH_PROMPT_STATUS=")
                || command.starts_with("_COSH_PROMPT_DEBUG_TRAP=")
                || command.starts_with("_COSH_PROMPT_RETURN_TRAP=")
                || command.starts_with("_COSH_PROMPT_ERR_TRAP=")
                || command == "_COSH_PROMPT_XTRACE=0"
                || command == "_COSH_PROMPT_XTRACE=1"
                || command == "(( _COSH_PROMPT_XTRACE == 1 ))"
                || command == "unset _COSH_PROMPT_XTRACE"
                || (command.starts_with("trap -p DEBUG >")
                    && command.contains("COSH_RECOVERY_REQUEST_FILE")),
            "unexpected Cosh internals in DEBUG trap: {line}\n{debug_log}"
        );
    }
}

fn assert_bash_xtrace_has_only_bounded_cosh_entries(trace: &str) {
    let mut cosh_lines = 0;
    for line in trace
        .lines()
        .filter(|line| line.contains("_cosh") || line.contains("_COSH"))
    {
        cosh_lines += 1;
        let command = line
            .split_once("BASH_USER_TRACE__ ")
            .map(|(_, command)| command)
            .unwrap_or(line);
        let prompt_status = command
            .strip_prefix("_COSH_PROMPT_STATUS=")
            .is_some_and(|status| {
                !status.is_empty() && status.bytes().all(|byte| byte.is_ascii_digit())
            });
        assert!(
            prompt_status
                || matches!(
                    command,
                    "_COSH_PROMPT_XTRACE=0"
                        | "_COSH_PROMPT_XTRACE=1"
                        | "local _COSH_COMMAND_NOT_FOUND_XTRACE=0"
                        | "local _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE=0"
                        | "_COSH_COMMAND_NOT_FOUND_XTRACE=1"
                        | "_COSH_COMMAND_NOT_FOUND_FINAL_XTRACE=1"
                ),
            "unexpected Cosh xtrace entry: {line}\n{trace}"
        );
    }
    assert!(cosh_lines <= 64, "unbounded Cosh xtrace entries: {trace}");
}

#[test]
fn native_integration_leaves_bash_hooks_and_input_owned_by_bash() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    for enable_bracketed_paste in [false, true] {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-native-integration-{}-{}-{}",
            enable_bracketed_paste,
            std::process::id(),
            unique_suffix()
        ));
        let home_dir = work_dir.join("home");
        std::fs::create_dir_all(&home_dir).expect("home dir");
        std::fs::write(
            home_dir.join(".bashrc"),
            r#"PROMPT_COMMAND='printf "__USER_PROMPT_COMMAND__\n"'
trap 'printf "%s\n" "$BASH_COMMAND" >> "$HOME/debug-trap.log"' DEBUG
trap 'printf "%s\n" "$BASH_COMMAND" >> "$HOME/return-trap.log"' RETURN
trap 'printf "%s\n" "$BASH_COMMAND" >> "$HOME/err-trap.log"' ERR
"#,
        )
        .expect("bashrc");
        let config = ShellHostConfig::new("native-integration", &work_dir)
            .with_integration(ShellIntegration::Native)
            .with_env("HOME", home_dir.display().to_string())
            .with_env("PATH", "/usr/bin:/bin");
        let config = with_bracketed_paste_readline(config, enable_bracketed_paste);
        assert_eq!(config.integration, ShellIntegration::Native);

        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_actions(
            &config,
            vec![
                RawRelayAction::wait(Duration::from_millis(100)),
                RawRelayAction::line("printf '__FLAGS__=%s\\n' \"$-\""),
                RawRelayAction::line(
                    "shopt -q extdebug; printf '__EXTDEBUG_STATUS__=%s\\n' \"$?\"",
                ),
                RawRelayAction::line(
                    "set -o | { grep -E '^(errtrace|functrace)[[:space:]]' || :; }",
                ),
                RawRelayAction::line("printf '__DEBUG_TRAP__=%q\\n' \"$(trap -p DEBUG)\""),
                RawRelayAction::line(
                    "printf '__COSH_SESSION_ID__=%s\\n' \"${COSH_SESSION_ID-unset}\"",
                ),
                RawRelayAction::line("set -x"),
                RawRelayAction::line("printf '__XTRACE_ALIVE__\\n'"),
                RawRelayAction::line("set +x"),
                RawRelayAction::line("hello"),
                RawRelayAction::line("/"),
                RawRelayAction::line("set -f; ??; set +f"),
                RawRelayAction::wait(Duration::from_millis(200)),
                RawRelayAction::line("exit"),
            ],
            &mut rendered,
        )
        .expect("native bash relay");

        let raw_terminal = String::from_utf8_lossy(&rendered);
        let terminal = without_readline_mode_controls(&raw_terminal);
        let flags = terminal
            .lines()
            .find_map(|line| {
                let start = line.find("__FLAGS__=")?;
                let value = line[start + "__FLAGS__=".len()..].trim_end_matches('\r');
                (!value.is_empty()
                    && value
                        .chars()
                        .all(|character| character.is_ascii_alphabetic()))
                .then_some(value)
            })
            .expect("shell flags");
        assert!(!flags.contains('E'), "{terminal}");
        assert!(!flags.contains('T'), "{terminal}");
        assert!(terminal.contains("__EXTDEBUG_STATUS__=1"), "{terminal}");
        assert!(
            terminal.contains("errtrace") && terminal.contains("off"),
            "{terminal}"
        );
        assert!(
            terminal.contains("functrace") && terminal.contains("off"),
            "{terminal}"
        );
        assert!(terminal.contains("__USER_PROMPT_COMMAND__"), "{terminal}");
        assert!(terminal.contains("__XTRACE_ALIVE__"), "{terminal}");
        assert!(terminal.contains("__COSH_SESSION_ID__=unset"), "{terminal}");
        assert!(!terminal.contains("_cosh"), "{terminal}");
        assert!(!terminal.contains("COSH_MARKER_TOKEN"), "{terminal}");
        assert!(!work_dir.join("cosh-marker.bash").exists());
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && matches!(event.input.as_deref(), Some("hello" | "/" | "??"))
        }));
        assert!(
            output.events.iter().all(|event| {
                matches!(
                    event.kind,
                    ShellEventKind::ShellStarted | ShellEventKind::ShellExited
                )
            }),
            "{:?}",
            output.events
        );

        for trap_log in ["debug-trap.log", "return-trap.log", "err-trap.log"] {
            let content = std::fs::read_to_string(home_dir.join(trap_log)).unwrap_or_default();
            assert!(!content.contains("_cosh"), "{trap_log}: {content}");
            assert!(
                !content.contains("COSH_MARKER_TOKEN"),
                "{trap_log}: {content}"
            );
        }

        let _ = std::fs::remove_dir_all(&work_dir);
    }
}

#[test]
fn enhanced_assisted_integration_remains_the_default() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-integration-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    let config = ShellHostConfig::new("enhanced-integration", &work_dir)
        .with_env("HOME", home_dir.display().to_string());
    assert_eq!(config.integration, ShellIntegration::Enhanced);
    let output = shell_run_scripted_bash(
        &config,
        &[
            ScriptedInput::user_line("hello"),
            ScriptedInput::user_line("hello there"),
        ],
    )
    .expect("enhanced bash session");

    assert!(work_dir.join("cosh-marker.bash").is_file());
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("hello")
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("hello there")
    }));

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_routes_without_global_debug_tracing() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-bounded-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        r#"PS1='v2$ '
PS0='__USER_PS0__'
shopt -u extdebug
set +E +T
trap 'printf "%s\n" "$BASH_COMMAND" >> "$HOME/debug-trap.log"' DEBUG
"#,
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("enhanced-bounded", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("LANG", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8");

    let mut rendered = Vec::new();
    let output = shell_run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("shopt -q extdebug; printf '__EXTDEBUG__=%s\\n' \"$?\""),
            RawRelayAction::line("set -o | { grep -E '^(errtrace|functrace)[[:space:]]' || :; }"),
            RawRelayAction::line("printf '__DEBUG__=%s\\n' \"$(trap -p DEBUG)\""),
            RawRelayAction::line("printf '__PUBLIC_TOKEN__=%s\\n' \"${COSH_MARKER_TOKEN-unset}\""),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("hello there"),
            RawRelayAction::wait(Duration::from_millis(300)),
            // Classify Bash's accepted Readline line, including edits, rather
            // than relying on DEBUG-trap BASH_COMMAND state.
            RawRelayAction::write(b"please helx\x7fp\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("请帮我分析"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("missing-cosh-v2-command"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("set -x"),
            RawRelayAction::line(
                "K=XTRACE; case $- in *x*) V=PRESERVED;; *) V=LOST;; esac; printf '__%s_%s__\\n' \"$K\" \"$V\"",
            ),
            RawRelayAction::line("set +x"),
            RawRelayAction::line("set -u"),
            RawRelayAction::line(":"),
            RawRelayAction::line(":"),
            RawRelayAction::line("printf '__NOUNSET_ALIVE__\\n'"),
            RawRelayAction::line("set +u"),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced v2 bash relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("__USER_PS0__"), "{terminal}");
    assert!(terminal.contains("__EXTDEBUG__=1"), "{terminal}");
    assert!(
        terminal.contains("errtrace") && terminal.contains("off"),
        "{terminal}"
    );
    assert!(
        terminal.contains("functrace") && terminal.contains("off"),
        "{terminal}"
    );
    assert!(terminal.contains("__PUBLIC_TOKEN__=unset"), "{terminal}");
    assert!(terminal.contains("__XTRACE_PRESERVED__"), "{terminal}");
    assert!(terminal.contains("__NOUNSET_ALIVE__"), "{terminal}");
    assert!(
        !terminal.contains("HISTTIMEFORMAT: unbound variable"),
        "{terminal}"
    );
    let assistance_state_path = work_dir.join("assistance-enabled").display().to_string();
    assert!(
        !terminal.contains(&assistance_state_path),
        "assistance state path leaked through xtrace: {terminal}"
    );
    assert!(
        terminal.contains("missing-cosh-v2-command: command not found"),
        "{terminal}"
    );
    assert!(terminal.contains("trap -- 'printf"), "{terminal}");

    let marker =
        std::fs::read_to_string(work_dir.join("cosh-marker.bash")).expect("enhanced v2 marker");
    let marker_token = marker
        .lines()
        .find_map(|line| line.strip_prefix("COSH_MARKER_TOKEN='")?.strip_suffix('\''))
        .expect("marker token");
    assert!(
        !terminal.contains(marker_token),
        "marker token leaked: {terminal}"
    );

    for input in ["hello there", "please help", "请帮我分析"] {
        let intercepted = output
            .events
            .iter()
            .filter(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input)
                    && event.component.as_deref() == Some("natural_language")
            })
            .count();
        assert_eq!(intercepted, 1, "unexpected routes for {input:?}");
    }
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("missing-cosh-v2-command")
    }));

    let debug_log =
        std::fs::read_to_string(home_dir.join("debug-trap.log")).expect("user DEBUG trap log");
    assert!(!debug_log.contains("_cosh_preexec_marker"), "{debug_log}");
    assert!(!debug_log.contains(marker_token), "{debug_log}");
    assert_debug_log_has_only_bounded_cosh_capture(&debug_log);

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_matches_bash_trap_and_option_oracle() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-oracle-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        r#"PS1='oracle$ '
PS0='__ORACLE_PS0__'
PROMPT_COMMAND='printf "__USER_PROMPT__=%s\n" "$?"'
shopt -u extdebug
set +E +T
"#,
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("enhanced-oracle", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("LANG", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8");

    let mut rendered = Vec::new();
    let output = shell_run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(
                "trap 'printf \"DBG=[%s]\\n\" \"$BASH_COMMAND\" >> \"$HOME/debug.log\"' DEBUG",
            ),
            RawRelayAction::line(
                "trap 'printf \"RET=[%s]\\n\" \"$BASH_COMMAND\" >> \"$HOME/return.log\"' RETURN",
            ),
            RawRelayAction::line(
                "trap 'printf \"ERR=[%s]\\n\" \"$BASH_COMMAND\" >> \"$HOME/err.log\"' ERR",
            ),
            RawRelayAction::line(": > \"$HOME/debug.log\""),
            RawRelayAction::line("echo user-visible-cmd"),
            RawRelayAction::line("printf '__DASH__=%s\\n' \"$-\""),
            RawRelayAction::line("set -o | { grep -E '^(errtrace|functrace)[[:space:]]' || :; }"),
            RawRelayAction::line("shopt -q extdebug; printf '__EXTDEBUG__=%s\\n' \"$?\""),
            RawRelayAction::line("printf '__DEBUG_TRAP__=%s\\n' \"$(trap -p DEBUG)\""),
            RawRelayAction::line(
                "printf '__PROMPT_COMMAND__=%s\\n' \"$(declare -p PROMPT_COMMAND)\"",
            ),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced v2 bash oracle");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("user-visible-cmd"), "{terminal}");
    assert!(terminal.contains("__ORACLE_PS0__"), "{terminal}");
    assert!(terminal.contains("__USER_PROMPT__"), "{terminal}");
    assert!(terminal.contains("__EXTDEBUG__=1"), "{terminal}");
    assert!(
        terminal.contains("errtrace") && terminal.contains("off"),
        "{terminal}"
    );
    assert!(
        terminal.contains("functrace") && terminal.contains("off"),
        "{terminal}"
    );
    assert!(terminal.contains("trap -- 'printf"), "{terminal}");
    let prompt_command_declaration = if bash_supports_prompt_command_array() {
        "__PROMPT_COMMAND__=declare -a PROMPT_COMMAND="
    } else {
        "__PROMPT_COMMAND__=declare -- PROMPT_COMMAND="
    };
    assert!(
        terminal.contains(prompt_command_declaration)
            && terminal.contains("printf \\\"__USER_PROMPT__=%s"),
        "{terminal}"
    );

    let marker =
        std::fs::read_to_string(work_dir.join("cosh-marker.bash")).expect("enhanced v2 marker");
    let marker_token = marker
        .lines()
        .find_map(|line| line.strip_prefix("COSH_MARKER_TOKEN='")?.strip_suffix('\''))
        .expect("marker token");
    let debug_log = std::fs::read_to_string(home_dir.join("debug.log")).expect("DEBUG trap log");
    let return_log = std::fs::read_to_string(home_dir.join("return.log")).unwrap_or_default();
    let err_log = std::fs::read_to_string(home_dir.join("err.log")).unwrap_or_default();
    for trap_log in [&debug_log, &return_log, &err_log] {
        assert!(!trap_log.contains(marker_token), "{trap_log}");
    }
    assert!(!return_log.contains("_cosh"), "{return_log}");
    assert!(!err_log.contains("_cosh"), "{err_log}");
    assert_debug_log_has_only_bounded_cosh_capture(&debug_log);
    assert!(
        debug_log.contains("DBG=[echo user-visible-cmd]"),
        "{debug_log}"
    );
    assert!(return_log.is_empty(), "{return_log}");
    assert_eq!(err_log, "ERR=[shopt -q extdebug]\n", "{err_log}");
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event.command.as_deref() == Some("echo user-visible-cmd")
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandCompleted
            && event.command.as_deref() == Some("echo user-visible-cmd")
            && event.exit_code == Some(0)
    }));

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_bash_functrace_keeps_prompt_internals_bounded() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-functrace-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        r#"PS1='functrace$ '
set -T
trap 'printf "DBG=[%s]\n" "$BASH_COMMAND" >> "$HOME/functrace.log"' DEBUG
"#,
    )
    .expect("bashrc");
    let sensitive_path = "/cosh-functrace-private-2683:/usr/bin:/bin";
    let config = ShellHostConfig::new("enhanced-functrace", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("PATH", sensitive_path);

    let mut rendered = Vec::new();
    run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("set -x"),
            RawRelayAction::line("printf '__FUNCTRACE_USER_COMMAND__\\n'"),
            RawRelayAction::line("set +x"),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced Bash functrace relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(
        terminal.contains("__FUNCTRACE_USER_COMMAND__"),
        "{terminal}"
    );
    let debug_log =
        std::fs::read_to_string(home_dir.join("functrace.log")).expect("functrace DEBUG log");
    assert_debug_log_has_only_bounded_cosh_capture(&debug_log);
    assert!(!debug_log.contains("_cosh_"), "{debug_log}");
    assert!(!debug_log.contains(sensitive_path), "{debug_log}");
    assert!(
        !debug_log.contains(work_dir.to_str().expect("UTF-8 work dir")),
        "{debug_log}"
    );
    let marker =
        std::fs::read_to_string(work_dir.join("cosh-marker.bash")).expect("enhanced Bash marker");
    let marker_token = marker
        .lines()
        .find_map(|line| line.strip_prefix("COSH_MARKER_TOKEN='")?.strip_suffix('\''))
        .expect("marker token");
    assert!(!debug_log.contains(marker_token), "{debug_log}");

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_prompt_wrappers_preserve_user_variables() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-prompt-vars-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "d=initial-d\n\
         r=initial-r\n\
         e=initial-e\n\
         s=initial-s\n\
         __prompt_status=initial-status\n\
         PROMPT_COMMAND='d=hook-d; r=hook-r; e=hook-e; s=hook-s; \
         __prompt_status=hook-status'\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("enhanced-prompt-vars", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string());
    let output = run_scripted_bash(
        &config,
        &[ScriptedInput::user_line(
            "printf '__PROMPT_VARS__=%s|%s|%s|%s|%s\\n' \
             \"$d\" \"$r\" \"$e\" \"$s\" \"$__prompt_status\"",
        )],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        terminal.contains("__PROMPT_VARS__=hook-d|hook-r|hook-e|hook-s|hook-status"),
        "{terminal}"
    );
}

#[test]
fn enhanced_prompt_wrappers_do_not_assign_readonly_user_variables() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-readonly-prompt-vars-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "readonly d=readonly-d\n\
         readonly r=readonly-r\n\
         readonly e=readonly-e\n\
         readonly s=readonly-s\n\
         PROMPT_COMMAND='printf \"__READONLY_PROMPT_HOOK__\\n\"'\n",
    )
    .expect("bashrc");

    let config = ShellHostConfig::new("enhanced-readonly-prompt-vars", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("LANG", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8");
    let output = run_scripted_bash(
        &config,
        &[ScriptedInput::user_line(
            "printf '__READONLY_VARS__=%s|%s|%s|%s\\n' \"$d\" \"$r\" \"$e\" \"$s\"",
        )],
    )
    .expect("scripted bash pty");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("__READONLY_PROMPT_HOOK__"), "{terminal}");
    assert!(
        terminal.contains("__READONLY_VARS__=readonly-d|readonly-r|readonly-e|readonly-s"),
        "{terminal}"
    );
    assert!(!terminal.contains("readonly variable"), "{terminal}");
}

#[test]
fn enhanced_prompt_boundary_survives_user_ps1_reassignment() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-prompt-reset-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "PS1='initial$ '\n\
         PROMPT_COMMAND='PS1=\"reset$ \"; printf \"__RESET_HOOK__\\n\"'\n",
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("enhanced-prompt-reset", &work_dir)
        .with_env("HOME", home_dir.display().to_string());

    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("echo first-boundary"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("echo second-boundary"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced prompt reset relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("__RESET_HOOK__"), "{terminal}");
    assert!(terminal.contains("reset$ "), "{terminal}");
    for command in ["echo first-boundary", "echo second-boundary"] {
        assert!(
            output.events.iter().any(|event| {
                event.kind == ShellEventKind::CommandCompleted
                    && event.command.as_deref() == Some(command)
            }),
            "missing boundary for {command}: {:?}",
            output.events
        );
    }

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_history_disabled_emits_redacted_boundaries_and_keeps_shell_ownership() {
    if Command::new("bash").arg("--version").output().is_err()
        || !bash_supports_command_not_found_handler()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-no-history-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "PS1='no-history$ '\nset +o history\n",
    )
    .expect("bashrc");
    let config = ShellHostConfig::new("enhanced-no-history", &work_dir)
        .with_env("HOME", home_dir.display().to_string());

    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("printf '__NO_HISTORY_COMMAND__\\n'"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("hello there"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced no-history relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("__NO_HISTORY_COMMAND__"), "{terminal}");
    assert!(terminal.contains("hello: command not found"), "{terminal}");
    assert!(
        output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandCompleted
                && event.command.as_deref() == Some("<redacted untracked command>")
        }),
        "{:?}",
        output.events
    );
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("hello there")
    }));

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_shift_tab_toggles_shell_only_routing_without_restarting_bash() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    for enable_bracketed_paste in [false, true] {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-enhanced-toggle-{}-{}-{}",
            enable_bracketed_paste,
            std::process::id(),
            unique_suffix()
        ));
        let home_dir = work_dir.join("home");
        std::fs::create_dir_all(&home_dir).expect("home dir");
        std::fs::write(home_dir.join(".bashrc"), "PS1='switch$ '\nKEEP_ME=alive\n")
            .expect("bashrc");
        let config = ShellHostConfig::new("enhanced-toggle", &work_dir)
            .with_integration(ShellIntegration::Enhanced)
            .with_env("HOME", home_dir.display().to_string());
        let config = with_bracketed_paste_readline(config, enable_bracketed_paste);

        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_actions(
            &config,
            vec![
                RawRelayAction::wait(Duration::from_millis(100)),
                RawRelayAction::write(b"\x1b[Z".to_vec()),
                RawRelayAction::line("hello there"),
                RawRelayAction::wait(Duration::from_millis(300)),
                RawRelayAction::line("/help"),
                RawRelayAction::wait(Duration::from_millis(300)),
                RawRelayAction::line("printf '__KEEP__=%s\\n' \"$KEEP_ME\""),
                // The toggle is valid only after the command boundary has
                // repainted an empty prompt. Leave enough time for a loaded
                // CI runner to observe and render that boundary.
                RawRelayAction::wait(Duration::from_secs(1)),
                RawRelayAction::write(b"\x1b[Z".to_vec()),
                RawRelayAction::line("hello there"),
                RawRelayAction::wait(Duration::from_millis(100)),
                RawRelayAction::line("exit"),
            ],
            &mut rendered,
        )
        .expect("enhanced toggle relay");

        let raw_terminal = String::from_utf8_lossy(&rendered);
        let terminal = without_readline_mode_controls(&raw_terminal);
        assert!(terminal.contains("\r\x1b[2Kswitch$ "), "{terminal}");
        assert!(
            !terminal.contains('◇') && !terminal.contains('◌'),
            "ownership toggles must not emit status symbol lines: {terminal}"
        );
        assert!(terminal.contains("__KEEP__=alive"), "{terminal}");
        let intercepted = output
            .events
            .iter()
            .filter(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some("hello there")
            })
            .count();
        assert_eq!(intercepted, 1, "{:?}", output.events);
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("/help")
        }));

        let _ = std::fs::remove_dir_all(&work_dir);
    }
}

#[test]
fn native_integration_leaves_zsh_startup_and_input_owned_by_zsh() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-native-zsh-integration-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(home_dir.join(".zshrc"), "printf '__USER_ZSHRC__\\n'\n").expect("zshrc");
    let config = ShellHostConfig::new("native-zsh-integration", &work_dir)
        .with_integration(ShellIntegration::Native)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("ZDOTDIR", home_dir.display().to_string());

    let mut rendered = Vec::new();
    let output = shell_run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("hello"),
            RawRelayAction::line("/"),
            RawRelayAction::line("setopt NO_NOMATCH; ??"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("native zsh relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("__USER_ZSHRC__"), "{terminal}");
    assert!(!terminal.contains("_cosh"), "{terminal}");
    assert!(!work_dir.join(".zshrc").exists());
    assert!(output.events.iter().all(|event| {
        matches!(
            event.kind,
            ShellEventKind::ShellStarted | ShellEventKind::ShellExited
        )
    }));

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_zsh_xtrace_hides_assistance_state_path() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-zsh-xtrace-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(home_dir.join(".zshrc"), "PS1='zsh-xtrace% '\n").expect("zshrc");
    let config = ShellHostConfig::new("enhanced-zsh-xtrace", &work_dir)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("ZDOTDIR", home_dir.display().to_string());

    let mut rendered = Vec::new();
    run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("set -x"),
            RawRelayAction::line("_cosh_assistance_enabled"),
            RawRelayAction::line("set +x"),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced Zsh xtrace relay");

    let terminal = String::from_utf8_lossy(&rendered);
    let assistance_state_path = work_dir.join("assistance-enabled").display().to_string();
    assert!(
        !terminal.contains(&assistance_state_path),
        "assistance state path leaked through Zsh xtrace: {terminal}"
    );

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_bash_xtrace_bounds_internal_hooks_and_preserves_user_state() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-bash-bounded-xtrace-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".bashrc"),
        "PS1='bash-xtrace$ '\nPROMPT_COMMAND='printf \"__USER_PROMPT_HOOK__\\n\"'\n\
         command_not_found_handle() {\n\
           case \"$1\" in\n\
             toggle-off) set +x; return 41;;\n\
             toggle-on) set -x; return 42;;\n\
             *) printf '__USER_BASH_MISSING__\\n'; return 43;;\n\
           esac\n\
         }\n",
    )
    .expect("bashrc");
    let trace_path = home_dir.join("xtrace.log");
    let sensitive_path = "/cosh-private-path-2683:/usr/bin:/bin";
    let config = ShellHostConfig::new("enhanced-bash-bounded-xtrace", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("PATH", sensitive_path);

    let mut rendered = Vec::new();
    run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!("exec 9>{}", trace_path.display())),
            RawRelayAction::line("BASH_XTRACEFD=9; PS4='__BASH_USER_TRACE__ '"),
            RawRelayAction::line("set -x"),
            RawRelayAction::line("printf '__BASH_USER_COMMAND__\\n'"),
            RawRelayAction::line(
                "missing-cosh-xtrace-command; printf '__BASH_MISSING_STATUS__=%s\\n' \"$?\"",
            ),
            RawRelayAction::line(
                "case $- in *x*) printf '__BASH_XTRACE_ON__\\n';; *) printf '__BASH_XTRACE_LOST__\\n';; esac",
            ),
            RawRelayAction::line(
                "command_not_found_handle toggle-off; s=$?; case $- in *x*) f=on;; *) f=off;; esac; printf '__BASH_HANDLER_OFF__=%s:%s\\n' \"$s\" \"$f\"",
            ),
            RawRelayAction::line(
                "command_not_found_handle toggle-on; s=$?; case $- in *x*) f=on;; *) f=off;; esac; printf '__BASH_HANDLER_ON__=%s:%s\\n' \"$s\" \"$f\"; set +x",
            ),
            RawRelayAction::line("printf '__BASH_PS4__=%s\\n' \"$PS4\""),
            RawRelayAction::line("printf '__BASH_XTRACEFD__=%s\\n' \"$BASH_XTRACEFD\""),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced Bash bounded xtrace relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("__BASH_USER_COMMAND__"), "{terminal}");
    assert!(terminal.contains("__USER_BASH_MISSING__"), "{terminal}");
    assert!(
        terminal.contains("__BASH_MISSING_STATUS__=43"),
        "{terminal}"
    );
    assert!(terminal.contains("__BASH_XTRACE_ON__"), "{terminal}");
    assert!(!terminal.contains("__BASH_XTRACE_LOST__\r\n"), "{terminal}");
    assert!(
        terminal.contains("__BASH_HANDLER_OFF__=41:off"),
        "{terminal}"
    );
    assert!(terminal.contains("__BASH_HANDLER_ON__=42:on"), "{terminal}");
    assert!(
        terminal.contains("__BASH_PS4__=__BASH_USER_TRACE__ "),
        "{terminal}"
    );
    assert!(terminal.contains("__BASH_XTRACEFD__=9"), "{terminal}");

    let trace = std::fs::read_to_string(&trace_path).expect("Bash xtrace log");
    assert!(trace.contains("__BASH_USER_COMMAND__"), "{trace}");
    assert!(trace.contains("__USER_PROMPT_HOOK__"), "{trace}");
    assert!(
        trace
            .lines()
            .any(|line| line.contains("printf '__USER_BASH_MISSING__")),
        "{trace}"
    );
    assert_bash_xtrace_has_only_bounded_cosh_entries(&trace);
    for private_value in [sensitive_path, work_dir.to_str().expect("UTF-8 work dir")] {
        assert!(
            !trace.contains(private_value),
            "private value leaked: {trace}"
        );
    }
    for internal in [
        "_cosh_json_escape",
        "_cosh_emit_marker",
        "_cosh_emit_boundary_marker",
        "_cosh_precmd_marker",
        "COSH_SESSION_ID",
        "_COSH_MARKER_TOKEN",
    ] {
        assert!(!trace.contains(internal), "internal xtrace leaked: {trace}");
    }
    let marker =
        std::fs::read_to_string(work_dir.join("cosh-marker.bash")).expect("enhanced Bash marker");
    let marker_token = marker
        .lines()
        .find_map(|line| line.strip_prefix("COSH_MARKER_TOKEN='")?.strip_suffix('\''))
        .expect("marker token");
    assert!(
        !trace.contains(marker_token),
        "marker token leaked: {trace}"
    );

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn enhanced_zsh_xtrace_bounds_internal_hooks_and_preserves_user_state() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-enhanced-zsh-bounded-xtrace-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home_dir = work_dir.join("home");
    std::fs::create_dir_all(&home_dir).expect("home dir");
    std::fs::write(
        home_dir.join(".zshrc"),
        "PS1='zsh-xtrace% '\n_user_precmd_2683() { print '__USER_ZSH_PRECMD__' }\n\
         precmd_functions+=(_user_precmd_2683)\n\
         command_not_found_handler() {\n\
           case \"$1\" in\n\
             toggle-off) set +x; return 44;;\n\
             toggle-on) set -x; return 45;;\n\
             *) print '__USER_ZSH_MISSING__'; return 46;;\n\
           esac\n\
         }\n",
    )
    .expect("zshrc");
    let sensitive_path = "/cosh-private-zsh-path-2683:/usr/bin:/bin";
    let config = ShellHostConfig::new("enhanced-zsh-bounded-xtrace", &work_dir)
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home_dir.display().to_string())
        .with_env("ZDOTDIR", home_dir.display().to_string())
        .with_env("PATH", sensitive_path);

    let mut rendered = Vec::new();
    run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("PS4='__ZSH_USER_TRACE__ '"),
            RawRelayAction::line("set -x"),
            RawRelayAction::line("print '__ZSH_USER_COMMAND__'"),
            RawRelayAction::line(
                "missing-cosh-zsh-xtrace-command; print '__ZSH_MISSING_STATUS__='$?",
            ),
            RawRelayAction::line(
                "if [[ -o xtrace ]]; then print '__ZSH_XTRACE_ON__'; else print '__ZSH_XTRACE_LOST__'; fi",
            ),
            RawRelayAction::line(
                "command_not_found_handler toggle-off; s=$?; print '__ZSH_HANDLER_OFF__='$s:${options[xtrace]}; set +x",
            ),
            RawRelayAction::line(
                "command_not_found_handler toggle-on; s=$?; print '__ZSH_HANDLER_ON__='$s:${options[xtrace]}",
            ),
            RawRelayAction::line("print -r -- \"__ZSH_PS4__=$PS4\""),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("enhanced Zsh bounded xtrace relay");

    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("__ZSH_USER_COMMAND__"), "{terminal}");
    assert!(terminal.contains("__USER_ZSH_PRECMD__"), "{terminal}");
    assert!(terminal.contains("__USER_ZSH_MISSING__"), "{terminal}");
    assert!(terminal.contains("__ZSH_MISSING_STATUS__=46"), "{terminal}");
    assert!(terminal.contains("__ZSH_XTRACE_ON__"), "{terminal}");
    assert!(!terminal.contains("__ZSH_XTRACE_LOST__\r\n"), "{terminal}");
    // zsh always restores XTRACE when a function returns, even without
    // LOCAL_OPTIONS. Match that native contract while preserving the status.
    assert!(terminal.contains("__ZSH_HANDLER_OFF__=44:on"), "{terminal}");
    assert!(terminal.contains("__ZSH_HANDLER_ON__=45:off"), "{terminal}");
    assert!(
        terminal.contains("__ZSH_PS4__=__ZSH_USER_TRACE__ "),
        "{terminal}"
    );
    assert!(
        !terminal.lines().any(|line| {
            line.contains("__ZSH_USER_TRACE__")
                && (line.contains("_cosh") || line.contains("_COSH"))
        }),
        "Cosh hook escaped the zsh xtrace guard: {terminal}"
    );
    for private_value in [sensitive_path, work_dir.to_str().expect("UTF-8 work dir")] {
        assert!(
            !terminal.contains(private_value),
            "private value leaked: {terminal}"
        );
    }
    for internal in [
        "_cosh_json_escape",
        "_cosh_emit_marker",
        "_cosh_precmd_marker:",
        "_cosh_preexec_marker:2",
        "_cosh_zshaddhistory_marker:2",
        "COSH_SESSION_ID",
    ] {
        assert!(
            !terminal.contains(internal),
            "internal xtrace leaked: {terminal}"
        );
    }
    let marker = std::fs::read_to_string(work_dir.join(".zshrc")).expect("enhanced Zsh marker");
    let marker_token = marker
        .lines()
        .find_map(|line| line.strip_prefix("COSH_MARKER_TOKEN='")?.strip_suffix('\''))
        .expect("marker token");
    assert!(
        !terminal.contains(marker_token),
        "marker token leaked: {terminal}"
    );

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn line_interactive_host_routes_input_to_bash_and_journal() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-line-host-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("line-host-test", &work_dir);
    let input = std::io::Cursor::new(
        "/explain last error\n\
         echo line-ok\n\
         please explain the last error\n\
         ls /path/that/does/not/exist\n",
    );
    let mut rendered = Vec::new();
    let output =
        run_line_interactive_bash(&config, input, &mut rendered).expect("line interactive host");

    let rendered_text = String::from_utf8_lossy(&output.rendered_output);
    assert!(!rendered_text.contains("intercepted  slash"));
    assert!(!rendered_text.contains("intercepted  natural_language"));
    assert!(rendered_text.contains("line-ok"));

    let replayed_events = read_shell_events(&output.shell.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo line-ok") && block.exit_code == 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("/path/that/does/not/exist") && block.exit_code != 0));
}

#[test]
fn line_interactive_host_can_invoke_claude_adapter_through_governance() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-line-claude-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("line-claude-test", &work_dir);
    let input = std::io::Cursor::new(
        "/explain last error\n\
         ls /path/that/does/not/exist\n",
    );
    let mut rendered = Vec::new();
    let output =
        run_line_interactive_bash(&config, input, &mut rendered).expect("line interactive host");

    let replayed_events = read_shell_events(&output.shell.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);

    let failed = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("/path/that/does/not/exist"))
        .expect("failed command block");
    let findings = findings_from_blocks(&ledger.blocks);
    let request = agent_request_after_confirmation("line-claude-test", failed, &findings, true)
        .expect("confirmed request");

    let agent_events = adapter_for_kind(AdapterKind::ClaudeCode)
        .run(&request)
        .expect("claude dry-run adapter");
    assert!(agent_events.iter().any(|event| matches!(
        event,
        AgentEvent::TextDelta { text, .. }
            if text.contains("Claude Code adapter prepared")
                && text.contains("--print")
    )));

    let governed = govern_agent_events(&agent_events, &Policy::default());
    assert!(governed.events.iter().all(|event| !event.auto_execute));
}

#[test]
fn line_interactive_host_runs_shell_command_with_non_ascii_path() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-line-unicode-path-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let file_name = "\u{8bbe}\u{8ba1}\u{6587}\u{6863}.md".to_string();
    let file_path = work_dir.join(&file_name);
    let file_content = "\u{4e2d}\u{6587}\u{5185}\u{5bb9}";
    std::fs::write(&file_path, file_content).expect("unicode file");

    let config = ShellHostConfig::new("line-unicode-path-test", &work_dir);
    let input = std::io::Cursor::new(format!("cat {}\necho after-cat\n", shell_arg(&file_path)));
    let mut rendered = Vec::new();
    let output =
        run_line_interactive_bash(&config, input, &mut rendered).expect("line interactive host");

    let rendered_text = String::from_utf8_lossy(&output.rendered_output);
    assert!(rendered_text.contains(file_content), "{rendered_text}");
    assert!(rendered_text.contains("after-cat"), "{rendered_text}");

    let replayed_events = read_shell_events(&output.shell.journal_path).expect("journal events");
    assert!(!replayed_events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));

    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("cat ") && block.exit_code == 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-cat") && block.exit_code == 0));
}
