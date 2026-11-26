use super::*;

fn configured_bash() -> String {
    std::env::var("COSH_TEST_BASH").unwrap_or_else(|_| "bash".to_string())
}

fn bash_lacks_ps0(program: &str) -> bool {
    Command::new(program)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "(( BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4 ))",
        ])
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn bash_capability_envelope_preserves_quoted_session_ids() {
    let program = configured_bash();
    let root = tempfile::tempdir().unwrap();
    let mut config =
        ShellHostConfig::new("session\"quote\\slash\ncontrol", root.path().join("work"))
            .with_integration(ShellIntegration::Enhanced)
            .with_env("LC_ALL", "C");
    config.native_mode = false;
    config.bash_path = program;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .unwrap();
    assert!(!output
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::ComponentFailed));
    assert_eq!(output.exit_status, Some(0));
}

#[test]
fn legacy_bash_routes_incremental_slash_without_command_start_claims() {
    let program = configured_bash();
    if !bash_lacks_ps0(&program) {
        eprintln!("SKIP: select Bash 4.2 or 4.3 with COSH_TEST_BASH");
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("cosh-legacy-bash-routing-")
        .tempdir()
        .expect("legacy routing root");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("home");
    std::fs::write(home.join(".bashrc"), "PS1='legacy$ '\n").expect("bashrc");
    let mut config = ShellHostConfig::new("legacy-routing", root.path().join("work"))
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home.display().to_string())
        .with_env("PATH", "/usr/bin:/bin")
        .with_env("LC_ALL", "C");
    config.bash_path = program;
    config.slash_via_shell = true;
    config.raw_action_watchdog = Duration::from_secs(10);
    let mut actions = vec![RawRelayAction::wait(Duration::from_millis(150))];
    for byte in b"/help\n" {
        actions.push(RawRelayAction::write(vec![*byte]));
        actions.push(RawRelayAction::wait(Duration::from_millis(30)));
    }
    actions.extend([
        RawRelayAction::wait(Duration::from_millis(250)),
        RawRelayAction::write(b"/hep\x1b[Dl\x1b[F\n".to_vec()),
        RawRelayAction::wait(Duration::from_millis(250)),
        RawRelayAction::write(b"\x1b[A\n".to_vec()),
        RawRelayAction::wait(Duration::from_millis(250)),
        RawRelayAction::line("Explain disk usage"),
        RawRelayAction::wait(Duration::from_millis(250)),
        RawRelayAction::line("(printf 'SUBSHELL_%s\\n' OK)"),
        RawRelayAction::wait(Duration::from_millis(150)),
        RawRelayAction::line("exit"),
    ]);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(&config, actions, &mut rendered)
        .expect("legacy routing relay");
    let terminal = String::from_utf8_lossy(&rendered);
    for input in ["/help", "Explain disk usage"] {
        assert!(
            output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input)
            }),
            "missing intercept for {input}: {terminal}"
        );
    }
    assert_eq!(
        output
            .events
            .iter()
            .filter(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some("/help")
            })
            .count(),
        3,
        "incremental, edited, and recalled slash must all be intercepted: {terminal}"
    );
    assert!(!terminal.contains("cannot find keymap"), "{terminal}");
    assert!(!terminal.contains("bash: /help"), "{terminal}");
    assert!(terminal.contains("SUBSHELL_OK"), "{terminal}");
    assert!(!output
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::CommandStarted));
    assert!(
        terminal.contains("command observation is unavailable"),
        "{terminal}"
    );
    assert_eq!(output.exit_status, Some(0), "{terminal}");
}

#[test]
fn legacy_bash_preserves_errexit_and_user_debug_trap() {
    let program = configured_bash();
    if !bash_lacks_ps0(&program) {
        eprintln!("SKIP: select Bash 4.2 or 4.3 with COSH_TEST_BASH");
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("cosh-legacy-bash-errexit-")
        .tempdir()
        .expect("legacy errexit root");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "PS1='legacy$ '\ntrap 'printf \"%s\\n\" \"$BASH_COMMAND\" >> \"$HOME/debug.log\"' DEBUG\n",
    )
    .expect("bashrc");
    let mut config = ShellHostConfig::new("legacy-errexit", root.path().join("work"))
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home.display().to_string())
        .with_env("PATH", "/usr/bin:/bin")
        .with_env("LC_ALL", "C");
    config.bash_path = program;
    config.raw_action_watchdog = Duration::from_secs(10);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::line("set -e; false || printf 'OR_%s\\n' RHS; printf 'AFTER_%s\\n' OK"),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::line("set -x"),
            RawRelayAction::line("printf 'TRACE_%s\\n' OK"),
            RawRelayAction::line("set +x"),
            RawRelayAction::line("trap - DEBUG; printf 'TRAP_%s\\n' CLEARED"),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("legacy errexit relay");
    let terminal = String::from_utf8_lossy(&rendered);
    assert!(terminal.contains("OR_RHS"), "{terminal}");
    assert!(terminal.contains("AFTER_OK"), "{terminal}");
    assert!(terminal.contains("TRAP_CLEARED"), "{terminal}");
    assert!(!terminal.contains("_cosh_bash42_preexec"), "{terminal}");
    assert!(!terminal.contains("COSH_MARKER_TOKEN="), "{terminal}");
    assert_eq!(output.exit_status, Some(0), "{terminal}");
    let debug = std::fs::read_to_string(home.join("debug.log")).expect("user debug log");
    assert!(debug.contains("false"), "{debug}");
    assert!(!debug.contains("_cosh_bash42_preexec"), "{debug}");
}

#[test]
fn modern_bash_keeps_the_native_shell_version_binding() {
    let program = configured_bash();
    if bash_lacks_ps0(&program) {
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("cosh-modern-bash-binding-")
        .tempdir()
        .expect("binding root");
    let mut config = ShellHostConfig::new("modern-binding", root.path().join("work"))
        .with_integration(ShellIntegration::Enhanced)
        .with_env("PATH", "/usr/bin:/bin")
        .with_env("LC_ALL", "C");
    config.native_mode = false;
    config.bash_path = program;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::line("bind -m emacs-standard -q display-shell-version"),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("modern binding relay");
    let terminal = String::from_utf8_lossy(&rendered);
    assert!(
        terminal.contains("display-shell-version can be invoked via"),
        "{terminal}"
    );
    assert!(terminal.contains("\\C-x\\C-v"), "{terminal}");
    assert!(
        !terminal.contains("command observation is unavailable"),
        "{terminal}"
    );
    assert_eq!(output.exit_status, Some(0), "{terminal}");
}

#[test]
fn legacy_bash_routes_accepted_input_not_excluded_or_imported_history() {
    let program = configured_bash();
    if !bash_lacks_ps0(&program) {
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("cosh-legacy-stale-history-")
        .tempdir()
        .expect("stale history root");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "PS1='stale$ '\nHISTIGNORE='Explain*'\nhistory -s 'Explain repeat an old task'\n",
    )
    .expect("bashrc");
    let mut config = ShellHostConfig::new("legacy-stale-history", root.path().join("work"))
        .with_integration(ShellIntegration::Enhanced)
        .with_env("HOME", home.display().to_string())
        .with_env("PATH", "/usr/bin:/bin")
        .with_env("LC_ALL", "C");
    config.bash_path = program;
    config.slash_via_shell = true;
    config.raw_action_watchdog = Duration::from_secs(10);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::line("Explain retry the current task"),
            RawRelayAction::wait(Duration::from_millis(250)),
            RawRelayAction::line("history -c"),
            RawRelayAction::wait(Duration::from_millis(250)),
            RawRelayAction::line("Explain with empty history"),
            RawRelayAction::wait(Duration::from_millis(250)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("stale history relay");
    let intercepted = output
        .events
        .iter()
        .filter_map(|event| {
            (event.kind == ShellEventKind::UserInputIntercepted
                && event.component.as_deref() == Some("natural_language"))
            .then_some(event.input.as_deref())
            .flatten()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        intercepted,
        [
            "Explain retry the current task",
            "Explain with empty history"
        ],
        "{}",
        String::from_utf8_lossy(&rendered)
    );
}
