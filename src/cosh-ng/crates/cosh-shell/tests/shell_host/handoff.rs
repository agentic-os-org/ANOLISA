use super::*;

/// Temporary Git repository plus a pager that blocks until a key arrives, so a
/// test can prove whether Git started a pager at all instead of guessing from
/// escape sequences.
struct ImplicitPagerFixture {
    repo: std::path::PathBuf,
    pager: std::path::PathBuf,
    sentinel: std::path::PathBuf,
    subject: String,
}

impl ImplicitPagerFixture {
    fn git_log_command(&self) -> String {
        format!(
            "git -C {} -c core.pager={} --paginate log -1 --format=%s",
            self.repo.display(),
            self.pager.display()
        )
    }

    fn git_log_command_using_only_the_environment_pager(&self) -> String {
        format!(
            "git -C {} -c core.pager= --paginate log -1 --format=%s",
            self.repo.display()
        )
    }
}

/// No surface a user reads or that gets persisted may carry the pager
/// environment cosh applies around an agent handoff. Matches the assignment form
/// the transport would use, which is what NON_INTERACTIVE_PAGER_PREFIX pins on
/// the Rust side; a bare variable name is legitimate in a command that reads it.
fn assert_no_pager_transport_leak(text: &str) {
    for marker in [
        "PAGER=cat",
        "GIT_PAGER=cat",
        "MANPAGER=cat",
        "SYSTEMD_PAGER=cat",
    ] {
        assert!(!text.contains(marker), "leaked {marker}: {text}");
    }
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn implicit_pager_fixture(label: &str) -> ImplicitPagerFixture {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-{label}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("fixture repository directory");

    let subject = format!("implicit-pager-subject-{}", unique_suffix());
    let commit_message = subject.clone();
    for args in [
        vec!["-c", "init.defaultBranch=main", "init", "--quiet"],
        vec!["config", "user.email", "pager-fixture@example.com"],
        vec!["config", "user.name", "Pager Fixture"],
        vec![
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            commit_message.as_str(),
        ],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .status()
            .expect("git fixture command");
        assert!(status.success(), "git {args:?}");
    }

    let sentinel = root.join("fake-pager-started");
    let pager = root.join("fake-pager");
    std::fs::write(
        &pager,
        format!(
            // Behaves like a real pager: paged text on stdin, keystrokes from
            // the controlling terminal, so it genuinely blocks until released.
            "#!/bin/sh\nprintf 'fake-pager-running\\n'\n: > '{}'\ncat\nIFS= read -r _key < /dev/tty\nprintf 'fake-pager-released\\n'\n",
            sentinel.display()
        ),
    )
    .expect("fake pager script");
    make_executable(&pager);

    ImplicitPagerFixture {
        repo,
        pager,
        sentinel,
        subject,
    }
}

/// Pins every variable the policy touches, so an assertion can neither be
/// satisfied nor broken by the developer's or CI's own pager configuration — a
/// host exporting `GIT_PAGER=cat` would otherwise make the disable case pass
/// without the policy under test. `env_overrides` can only set values, so the
/// "previously unset" branch is covered at the raw CLI layer, whose harness can
/// remove variables outright.
fn pin_pager_env(config: &mut ShellHostConfig, git_pager: &str) {
    for (name, value) in [
        ("PAGER", "user-pager"),
        ("GIT_PAGER", git_pager),
        ("MANPAGER", "user-manpager"),
        ("SYSTEMD_PAGER", "user-systemd-pager"),
    ] {
        config
            .env_overrides
            .push((name.to_string(), value.to_string()));
    }
}

fn pager_policy_handoff_request(command: &str, policy: ImplicitPagerPolicy) -> ShellHandoffRequest {
    let mut request = ShellHandoffRequest::new(
        command,
        format!("$ {command}"),
        "approved_provider_shell_tool",
        "user",
        "approval-pager",
        "run-pager",
        1,
    )
    .expect("handoff request");
    request.implicit_pager_policy = policy;
    request
}

fn emit_pager_policy_handoff<W: Write>(
    command: &str,
    policy: ImplicitPagerPolicy,
) -> impl FnMut(&[ShellEvent], &mut W) -> io::Result<RawObserverAction> {
    let command = command.to_string();
    let mut emitted = false;
    move |_, _| {
        if emitted {
            return Ok(RawObserverAction::Continue);
        }
        emitted = true;
        Ok(RawObserverAction::EmitToPty(pager_policy_handoff_request(
            &command, policy,
        )))
    }
}

/// Emits the handoff only once `setup_command` has finished, so a test can
/// establish shell state the handoff must preserve — such as a variable that is
/// set but deliberately not exported.
fn emit_pager_policy_handoff_after<W: Write>(
    setup_command: &str,
    command: &str,
    policy: ImplicitPagerPolicy,
) -> impl FnMut(&[ShellEvent], &mut W) -> io::Result<RawObserverAction> {
    let setup_command = setup_command.to_string();
    let command = command.to_string();
    let mut emitted = false;
    move |events, _| {
        if emitted {
            return Ok(RawObserverAction::Continue);
        }
        let setup_finished = events.iter().any(|event| {
            event.exit_code.is_some() && event.command.as_deref() == Some(setup_command.as_str())
        });
        if !setup_finished {
            return Ok(RawObserverAction::Continue);
        }
        emitted = true;
        Ok(RawObserverAction::EmitToPty(pager_policy_handoff_request(
            &command, policy,
        )))
    }
}

#[test]
fn raw_relay_bash_handoff_preserves_top_level_shell_state() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-top-level-handoff-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let command = "declare -x COSH_HANDOFF_STATE=alive; set -- first second";
    let config = ShellHostConfig::new("bash-top-level-handoff", &work_dir);
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line(
                "printf '__HANDOFF_STATE__=%s:%s:%s\\n' \"$COSH_HANDOFF_STATE\" \"$#\" \"$1\"",
            ),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff(command, ImplicitPagerPolicy::Inherit),
    )
    .expect("top-level Bash handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        terminal.contains("__HANDOFF_STATE__=alive:2:first"),
        "{terminal}"
    );
    assert!(
        output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandCompleted
                && event.command.as_deref() == Some(command)
                && event.exit_code == Some(0)
        }),
        "{:?}",
        output.events
    );

    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn raw_relay_bash_agent_git_handoff_does_not_enter_the_implicit_pager() {
    if Command::new("bash").arg("--version").output().is_err() || !git_available() {
        return;
    }

    let fixture = implicit_pager_fixture("bash-implicit-pager");
    let command = fixture.git_log_command();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-implicit-pager-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-implicit-pager-test", &work_dir);
    config.native_mode = false;
    // GIT_PAGER points at the blocking pager, so only the policy can keep Git
    // out of it — a host that already neutralized the pager cannot fake a pass.
    pin_pager_env(&mut config, &fixture.pager.to_string_lossy());
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(2_000)),
            RawRelayAction::line("history"),
            RawRelayAction::line("echo after-git"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff(&command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay bash implicit pager handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // No `q` was ever sent: reaching the next command at all proves the pager
    // never took the terminal.
    assert!(
        !fixture.sentinel.exists(),
        "implicit pager must not start: {terminal}"
    );
    assert!(!terminal.contains("fake-pager-running"), "{terminal}");
    assert!(terminal.contains(&fixture.subject), "{terminal}");
    assert!(terminal.contains("after-git"), "{terminal}");
    // The shell echoes the typed line and `history` replays it: neither may
    // show the transport environment.
    assert_no_pager_transport_leak(&terminal);

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == command)
        .unwrap_or_else(|| panic!("original handoff command block; terminal={terminal}"));
    assert_eq!(block.exit_code, 0, "{terminal}");
    assert_no_pager_transport_leak(&block.command);
    let output_ref = block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(output_text.contains(&fixture.subject), "{output_text}");
    assert_no_pager_transport_leak(&output_text);
}

#[test]
fn raw_relay_bash_pager_policy_does_not_outlive_the_handoff_command() {
    if Command::new("bash").arg("--version").output().is_err() || !git_available() {
        return;
    }

    let fixture = implicit_pager_fixture("bash-pager-scope");
    let command = fixture.git_log_command();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-pager-scope-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-pager-scope-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, &fixture.pager.to_string_lossy());
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(2_000)),
            RawRelayAction::line(
                "printf 'after=%s/%s/%s\\n' \"${PAGER-unset}\" \"${MANPAGER-unset}\" \"${SYSTEMD_PAGER-unset}\"",
            ),
            RawRelayAction::line("printf 'gitpager-after=%s\\n' \"${GIT_PAGER-unset}\""),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff(&command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay bash pager scope handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(!fixture.sentinel.exists(), "{terminal}");
    // Every variable the policy touched comes back to the user's own value for
    // their next command.
    assert!(
        terminal.contains("after=user-pager/user-manpager/user-systemd-pager"),
        "{terminal}"
    );
    assert!(
        terminal.contains(&format!("gitpager-after={}", fixture.pager.display())),
        "{terminal}"
    );
    assert!(!terminal.contains("after=cat"), "{terminal}");
    assert!(!terminal.contains("gitpager-after=cat"), "{terminal}");
}

#[test]
fn raw_relay_bash_handoff_keeps_a_pager_change_the_command_made_itself() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-pager-command-change-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-pager-command-change-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, "user-git-pager");
    // A forensics-classified command that deliberately reconfigures the pagers.
    let command = "export PAGER=command-chosen-pager; unset GIT_PAGER";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(1_500)),
            RawRelayAction::line("printf 'pager-after=%s\\n' \"${PAGER-unset}\""),
            RawRelayAction::line("printf 'gitpager-after=%s\\n' \"${GIT_PAGER-unset}\""),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff(command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay bash pager command change handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // Restoring the snapshot unconditionally would report success while silently
    // discarding what the command actually did.
    assert!(
        terminal.contains("pager-after=command-chosen-pager"),
        "{terminal}"
    );
    assert!(terminal.contains("gitpager-after=unset"), "{terminal}");
    assert!(!terminal.contains("pager-after=user-pager"), "{terminal}");
    assert!(
        !terminal.contains("gitpager-after=user-git-pager"),
        "{terminal}"
    );
}

#[test]
fn raw_relay_bash_handoff_keeps_an_export_attribute_change_the_command_made() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-pager-demote-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-pager-demote-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, "user-git-pager");
    // Drops the export attribute without changing the injected value, so a
    // value-only guard would still revert it.
    let command = "export -n PAGER";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(1_500)),
            RawRelayAction::line("printf 'pager-after=%s\\n' \"${PAGER-unset}\""),
            RawRelayAction::line("sh -c 'printf \"child-pager=%s\\n\" \"${PAGER-unset}\"'"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff(command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay bash pager demotion handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // The command asked for a shell-local `cat`, so that is what must survive —
    // not the user's exported value from before the handoff.
    assert!(terminal.contains("pager-after=cat"), "{terminal}");
    assert!(terminal.contains("child-pager=unset"), "{terminal}");
    assert!(!terminal.contains("pager-after=user-pager"), "{terminal}");
    assert!(!terminal.contains("child-pager=cat"), "{terminal}");
}

#[test]
fn raw_relay_bash_handoff_hides_an_exported_readonly_pager() {
    if Command::new("bash").arg("--version").output().is_err() || !git_available() {
        return;
    }

    let fixture = implicit_pager_fixture("bash-readonly-pager");
    let command = fixture.git_log_command_using_only_the_environment_pager();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-pager-readonly-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-pager-readonly-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, &fixture.pager.to_string_lossy());
    let setup = "readonly GIT_PAGER";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::line(setup),
            RawRelayAction::wait(Duration::from_millis(2_000)),
            RawRelayAction::line("echo after-readonly-git"),
            RawRelayAction::line("printf 'gitpager-after=%s\\n' \"${GIT_PAGER-unset}\""),
            RawRelayAction::line("sh -c 'printf \"child-gitpager=%s\\n\" \"${GIT_PAGER-unset}\"'"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff_after(setup, &command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay bash readonly pager handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        !fixture.sentinel.exists(),
        "readonly GIT_PAGER must stay out of Git's environment: {terminal}"
    );
    assert!(!terminal.contains("fake-pager-running"), "{terminal}");
    assert!(terminal.contains(&fixture.subject), "{terminal}");
    assert!(terminal.contains("after-readonly-git"), "{terminal}");
    assert!(
        terminal.contains(&format!("gitpager-after={}", fixture.pager.display())),
        "{terminal}"
    );
    assert!(
        terminal.contains(&format!("child-gitpager={}", fixture.pager.display())),
        "{terminal}"
    );
    // Assigning to the readonly value would print a localized diagnostic.
    assert!(!terminal.contains("GIT_PAGER: "), "{terminal}");
}

#[test]
fn raw_relay_bash_handoff_does_not_promote_a_shell_local_pager_to_the_environment() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-pager-local-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-pager-local-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, "user-git-pager");
    // Drop the exported value first: assigning over an exported variable would
    // keep the export attribute.
    let setup = "unset PAGER; PAGER=shell-local-pager";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::line(setup),
            RawRelayAction::wait(Duration::from_millis(2_000)),
            RawRelayAction::line("printf 'pager-after=%s\\n' \"${PAGER-unset}\""),
            RawRelayAction::line("sh -c 'printf \"child-pager=%s\\n\" \"${PAGER-unset}\"'"),
            RawRelayAction::line("sh -c 'printf \"child-manpager=%s\\n\" \"${MANPAGER-unset}\"'"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff_after(
            setup,
            "printf pager-local-handoff",
            ImplicitPagerPolicy::Disable,
        ),
    )
    .expect("raw relay bash shell-local pager handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("pager-local-handoff"), "{terminal}");
    assert!(
        terminal.contains("pager-after=shell-local-pager"),
        "{terminal}"
    );
    // Still not exported: a child process must not inherit what the user kept
    // shell-local before the handoff.
    assert!(terminal.contains("child-pager=unset"), "{terminal}");
    assert!(
        !terminal.contains("child-pager=shell-local-pager"),
        "{terminal}"
    );
    // And the reverse direction: a variable the user did export stays exported.
    assert!(
        terminal.contains("child-manpager=user-manpager"),
        "{terminal}"
    );
    assert!(!terminal.contains("child-manpager=unset"), "{terminal}");
}

#[test]
fn raw_relay_bash_user_typed_pager_assignments_are_not_a_handoff() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-user-pager-prefix-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-user-pager-prefix-test", &work_dir);
    config.native_mode = false;
    // A user command that happens to start with exactly the transport's pager
    // assignments must keep its full text everywhere.
    let command =
        "PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER=cat printf user-typed-prefix-ok";
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line(command),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
    )
    .expect("raw relay bash user-typed pager assignments");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("user-typed-prefix-ok"), "{terminal}");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("user-typed-prefix-ok"))
        .unwrap_or_else(|| panic!("user command block; terminal={terminal}"));
    assert_eq!(
        block.command, command,
        "the user's own assignments must not be stripped as a handoff prefix"
    );
    assert_eq!(block.exit_code, 0, "{terminal}");
}

#[test]
fn raw_relay_bash_inherit_policy_still_reaches_an_explicit_pager() {
    if Command::new("bash").arg("--version").output().is_err() || !git_available() {
        return;
    }

    let fixture = implicit_pager_fixture("bash-inherit-pager");
    let command = fixture.git_log_command();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-inherit-pager-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-inherit-pager-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, &fixture.pager.to_string_lossy());
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(2_000)),
            RawRelayAction::line("q"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff(&command, ImplicitPagerPolicy::Inherit),
    )
    .expect("raw relay bash inherit pager handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        fixture.sentinel.exists(),
        "inherited pager configuration must still start the pager: {terminal}"
    );
    assert!(terminal.contains("fake-pager-running"), "{terminal}");
    assert!(terminal.contains("fake-pager-released"), "{terminal}");
}

#[test]
fn raw_relay_zsh_agent_git_handoff_does_not_enter_the_implicit_pager() {
    if Command::new("zsh").arg("--version").output().is_err() || !git_available() {
        return;
    }

    let fixture = implicit_pager_fixture("zsh-implicit-pager");
    let command = fixture.git_log_command();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-implicit-pager-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-implicit-pager-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, &fixture.pager.to_string_lossy());
    let input = DelayedInput::new(vec![
        (b"history\n".to_vec(), Duration::from_millis(2_000)),
        (b"echo after-git\n".to_vec(), Duration::from_millis(400)),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let output = run_raw_relay_zsh_with_output_control(
        &config,
        input,
        Vec::new(),
        emit_pager_policy_handoff(&command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay zsh implicit pager handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        !fixture.sentinel.exists(),
        "implicit pager must not start: {terminal}"
    );
    assert!(!terminal.contains("fake-pager-running"), "{terminal}");
    assert!(terminal.contains(&fixture.subject), "{terminal}");
    assert!(terminal.contains("after-git"), "{terminal}");
    assert_no_pager_transport_leak(&terminal);

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == command)
        .unwrap_or_else(|| panic!("original zsh handoff command block; terminal={terminal}"));
    assert_eq!(block.exit_code, 0, "{terminal}");
    assert_no_pager_transport_leak(&block.command);
}

#[test]
fn raw_relay_zsh_handoff_hides_an_exported_readonly_pager() {
    if Command::new("zsh").arg("--version").output().is_err() || !git_available() {
        return;
    }

    let fixture = implicit_pager_fixture("zsh-readonly-pager");
    let command = fixture.git_log_command_using_only_the_environment_pager();
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-pager-readonly-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-pager-readonly-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, &fixture.pager.to_string_lossy());
    let setup = "readonly GIT_PAGER";
    let input = DelayedInput::new(vec![
        (
            format!("{setup}\n").into_bytes(),
            Duration::from_millis(600),
        ),
        (
            b"echo after-readonly-git\n".to_vec(),
            Duration::from_millis(2_000),
        ),
        (
            b"printf 'gitpager-after=%s\\n' \"${GIT_PAGER-unset}\"\n".to_vec(),
            Duration::from_millis(400),
        ),
        (
            b"sh -c 'printf \"child-gitpager=%s\\n\" \"${GIT_PAGER-unset}\"'\n".to_vec(),
            Duration::from_millis(400),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let output = run_raw_relay_zsh_with_output_control(
        &config,
        input,
        Vec::new(),
        emit_pager_policy_handoff_after(setup, &command, ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay zsh readonly pager handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(
        !fixture.sentinel.exists(),
        "readonly GIT_PAGER must stay out of Git's environment: {terminal}"
    );
    assert!(!terminal.contains("fake-pager-running"), "{terminal}");
    assert!(terminal.contains(&fixture.subject), "{terminal}");
    assert!(terminal.contains("after-readonly-git"), "{terminal}");
    assert!(
        terminal.contains(&format!("gitpager-after={}", fixture.pager.display())),
        "{terminal}"
    );
    assert!(
        terminal.contains(&format!("child-gitpager={}", fixture.pager.display())),
        "{terminal}"
    );
    assert!(!terminal.contains("GIT_PAGER: "), "{terminal}");
}

#[test]
fn raw_relay_zsh_handoff_keeps_an_export_attribute_change_the_command_made() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-pager-demote-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-pager-demote-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, "user-git-pager");
    let input = DelayedInput::new(vec![
        // Wake the relay so the initial observer can stage the handoff before
        // the first assertion-bearing command reaches the PTY.
        (b"\n".to_vec(), Duration::from_millis(100)),
        (
            b"printf 'pager-after=%s\\n' \"${PAGER-unset}\"\n".to_vec(),
            Duration::from_millis(4_000),
        ),
        (
            b"sh -c 'printf \"child-pager=%s\\n\" \"${PAGER-unset}\"'\n".to_vec(),
            Duration::from_millis(400),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let output = run_raw_relay_zsh_with_output_control(
        &config,
        input,
        Vec::new(),
        emit_pager_policy_handoff("typeset +x PAGER", ImplicitPagerPolicy::Disable),
    )
    .expect("raw relay zsh pager demotion handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("pager-after=cat"), "{terminal}");
    assert!(terminal.contains("child-pager=unset"), "{terminal}");
    assert!(!terminal.contains("pager-after=user-pager"), "{terminal}");
    assert!(!terminal.contains("child-pager=cat"), "{terminal}");
}

#[test]
fn raw_relay_zsh_handoff_restores_declaration_state_without_clobbering_the_command() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-pager-state-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-pager-state-test", &work_dir);
    config.native_mode = false;
    pin_pager_env(&mut config, "user-git-pager");
    // Drop the exported value first: assigning over an exported variable would
    // keep the export attribute.
    let setup = "unset PAGER; PAGER=shell-local-pager";
    let input = DelayedInput::new(vec![
        (
            format!("{setup}\n").into_bytes(),
            Duration::from_millis(600),
        ),
        (
            b"printf 'pager-after=%s\\n' \"${PAGER-unset}\"\n".to_vec(),
            Duration::from_millis(2_000),
        ),
        (
            b"sh -c 'printf \"child-pager=%s\\n\" \"${PAGER-unset}\"'\n".to_vec(),
            Duration::from_millis(400),
        ),
        (
            b"printf 'gitpager-after=%s\\n' \"${GIT_PAGER-unset}\"\n".to_vec(),
            Duration::from_millis(400),
        ),
        (
            b"printf 'manpager-after=%s\\n' \"${MANPAGER-unset}\"\n".to_vec(),
            Duration::from_millis(400),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let output = run_raw_relay_zsh_with_output_control(
        &config,
        input,
        Vec::new(),
        emit_pager_policy_handoff_after(
            setup,
            "export GIT_PAGER=command-chosen-pager",
            ImplicitPagerPolicy::Disable,
        ),
    )
    .expect("raw relay zsh pager declaration state handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    // Set but deliberately not exported before the handoff: restored as-is, and
    // still invisible to child processes.
    assert!(
        terminal.contains("pager-after=shell-local-pager"),
        "{terminal}"
    );
    assert!(terminal.contains("child-pager=unset"), "{terminal}");
    assert!(
        !terminal.contains("child-pager=shell-local-pager"),
        "{terminal}"
    );
    // The handoff command's own change survives, while a variable it never
    // touched returns to the user's exported value.
    assert!(
        terminal.contains("gitpager-after=command-chosen-pager"),
        "{terminal}"
    );
    assert!(
        terminal.contains("manpager-after=user-manpager"),
        "{terminal}"
    );
    assert!(!terminal.contains("manpager-after=cat"), "{terminal}");
}

#[test]
fn raw_relay_zsh_user_typed_pager_assignments_are_not_a_handoff() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-user-pager-prefix-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-user-pager-prefix-test", &work_dir);
    config.native_mode = false;
    let command =
        "PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER=cat printf user-typed-prefix-ok";
    let input = DelayedInput::new(vec![
        (
            format!("{command}\n").into_bytes(),
            Duration::from_millis(600),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let output = run_raw_relay_zsh_with_output_control(&config, input, Vec::new(), |_, _| {
        Ok(RawObserverAction::Continue)
    })
    .expect("raw relay zsh user-typed pager assignments");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("user-typed-prefix-ok"), "{terminal}");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("user-typed-prefix-ok"))
        .unwrap_or_else(|| panic!("user command block; terminal={terminal}"));
    assert_eq!(
        block.command, command,
        "the user's own assignments must not be stripped as a handoff prefix"
    );
}

#[test]
fn routing_c3_provider_no_regression_approved_handoff_does_not_leak() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-handoff-wrapper-leak-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("handoff-wrapper-leak-test", &work_dir);
    let mut emitted = false;
    let command = "printf handoff-visible";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        move |_, _| {
            if emitted {
                return Ok(RawObserverAction::Continue);
            }
            emitted = true;
            let request = ShellHandoffRequest::new(
                command,
                format!("$ {command}"),
                "approved_provider_shell_tool",
                "user",
                "approval-1",
                "run-1",
                1,
            )
            .expect("handoff request");
            Ok(RawObserverAction::EmitToPty(request))
        },
    )
    .expect("raw relay handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("handoff-visible"), "{terminal}");
    assert!(
        !terminal.contains("COSH_SHELL_HANDOFF_BYPASS"),
        "{terminal}"
    );

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == command)
        .expect("original handoff command block");
    assert_eq!(block.exit_code, 0, "{terminal}");
    assert_clean_shell_output_ref(block, "handoff-visible");
    let output_ref = block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(
        !output_text.contains("COSH_SHELL_HANDOFF_BYPASS"),
        "{output_text}"
    );
}

#[test]
fn raw_relay_bash_handoff_preserves_user_scratch_variables() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-handoff-vars-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("handoff-vars-test", &work_dir);
    let setup = "d=before-d; r=before-r; e=before-e; s=before-s; \
                 __prompt_status=before-status";
    let command = "d=after-d; r=after-r; e=after-e; s=after-s; \
                   __prompt_status=after-status; printf handoff-vars-updated";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line(setup),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line(
                "printf '__HANDOFF_VARS__=%s|%s|%s|%s|%s\\n' \
                 \"$d\" \"$r\" \"$e\" \"$s\" \"$__prompt_status\"",
            ),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff_after(setup, command, ImplicitPagerPolicy::Inherit),
    )
    .expect("raw relay bash handoff variables");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("handoff-vars-updated"), "{terminal}");
    assert!(
        terminal.contains("__HANDOFF_VARS__=after-d|after-r|after-e|after-s|after-status"),
        "{terminal}"
    );
}

#[test]
fn raw_relay_bash_handoff_does_not_assign_readonly_user_variables() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-handoff-readonly-vars-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("handoff-readonly-vars-test", &work_dir)
        .with_env("LANG", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8");
    let setup = "readonly d=readonly-d r=readonly-r e=readonly-e s=readonly-s";
    let command = "printf handoff-readonly-vars";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line(setup),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line(
                "printf '__HANDOFF_READONLY_VARS__=%s|%s|%s|%s\\n' \
                 \"$d\" \"$r\" \"$e\" \"$s\"",
            ),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        emit_pager_policy_handoff_after(setup, command, ImplicitPagerPolicy::Inherit),
    )
    .expect("raw relay bash readonly handoff variables");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("handoff-readonly-vars"), "{terminal}");
    assert!(
        terminal.contains("__HANDOFF_READONLY_VARS__=readonly-d|readonly-r|readonly-e|readonly-s"),
        "{terminal}"
    );
    assert!(!terminal.contains("readonly variable"), "{terminal}");
}

#[test]
fn raw_relay_handoff_provenance_does_not_set_child_environment() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-handoff-env-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("handoff-env-test", &work_dir);
    let mut emitted = false;
    let command = "sh -c 'printf \"handoff-bypass=%s\\n\" \"${COSH_SHELL_HANDOFF_BYPASS-unset}\"'";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        move |_, _| {
            if emitted {
                return Ok(RawObserverAction::Continue);
            }
            emitted = true;
            let request = ShellHandoffRequest::new(
                command,
                format!("$ {command}"),
                "approved_provider_shell_tool",
                "user",
                "approval-env",
                "run-env",
                1,
            )
            .expect("handoff request");
            Ok(RawObserverAction::EmitToPty(request))
        },
    )
    .expect("raw relay handoff env");

    let ledger = ledger_from_output(&output);
    let command_output = ledger_output_refs_text(&ledger);
    assert!(
        command_output.contains("handoff-bypass=unset"),
        "{command_output}"
    );
    assert!(
        !command_output.contains("handoff-bypass=1"),
        "{command_output}"
    );
}

#[test]
fn raw_relay_zsh_approved_handoff_wrapper_does_not_leak_to_output() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-handoff-wrapper-leak-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-handoff-wrapper-leak-test", &work_dir);
    config.native_mode = false;
    let input = DelayedInput::new(vec![(b"exit\n".to_vec(), Duration::from_millis(700))]);
    let mut emitted = false;
    let command = "printf zsh-handoff-visible";
    let output = run_raw_relay_zsh_with_output_control(&config, input, Vec::new(), move |_, _| {
        if emitted {
            return Ok(RawObserverAction::Continue);
        }
        emitted = true;
        let request = ShellHandoffRequest::new(
            command,
            format!("$ {command}"),
            "approved_provider_shell_tool",
            "user",
            "approval-1",
            "run-1",
            1,
        )
        .expect("handoff request");
        Ok(RawObserverAction::EmitToPty(request))
    })
    .expect("raw zsh relay handoff");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("zsh-handoff-visible"), "{terminal}");
    assert!(
        !terminal.contains("COSH_SHELL_HANDOFF_BYPASS"),
        "{terminal}"
    );

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == command)
        .expect("original zsh handoff command block");
    assert_eq!(block.exit_code, 0, "{terminal}");
    assert_clean_shell_output_ref(block, "zsh-handoff-visible");
    let output_ref = block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(
        !output_text.contains("COSH_SHELL_HANDOFF_BYPASS"),
        "{output_text}"
    );
}

#[test]
fn raw_relay_bash_history_records_original_handoff_command() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-handoff-history-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-handoff-history-test", &work_dir);
    config.native_mode = false;
    let mut emitted = false;
    let command = "printf bash-history-visible";
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line("history"),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        move |_, _| {
            if emitted {
                return Ok(RawObserverAction::Continue);
            }
            emitted = true;
            let request = ShellHandoffRequest::new(
                command,
                format!("$ {command}"),
                "approved_provider_shell_tool",
                "user",
                "approval-1",
                "run-1",
                1,
            )
            .expect("handoff request");
            Ok(RawObserverAction::EmitToPty(request))
        },
    )
    .expect("raw bash handoff history");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains(command), "{terminal}");
    assert!(
        !terminal.contains("COSH_SHELL_HANDOFF_BYPASS"),
        "{terminal}"
    );
}

#[test]
fn raw_relay_zsh_history_records_original_handoff_command() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-handoff-history-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-handoff-history-test", &work_dir);
    config.native_mode = false;
    let input = DelayedInput::new(vec![
        (b"history\n".to_vec(), Duration::from_millis(700)),
        (b"exit\n".to_vec(), Duration::from_millis(100)),
    ]);
    let mut emitted = false;
    let command = "printf zsh-history-visible";
    let output = run_raw_relay_zsh_with_output_control(&config, input, Vec::new(), move |_, _| {
        if emitted {
            return Ok(RawObserverAction::Continue);
        }
        emitted = true;
        let request = ShellHandoffRequest::new(
            command,
            format!("$ {command}"),
            "approved_provider_shell_tool",
            "user",
            "approval-1",
            "run-1",
            1,
        )
        .expect("handoff request");
        Ok(RawObserverAction::EmitToPty(request))
    })
    .expect("raw zsh handoff history");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains(command), "{terminal}");
    assert!(
        !terminal.contains("COSH_SHELL_HANDOFF_BYPASS"),
        "{terminal}"
    );
}

/// #2142 review (kongche P1): a queued unrelated command reaching preexec —
/// and, crucially, its precmd — between handoff staging and the handoff line
/// itself must not destroy the request/token sidecars. The handoff command
/// carries a secret, so after redaction the token echo is the only thing
/// keeping the block attributable; before the lifecycle fix the unrelated
/// precmd deleted both sidecars and the block downgraded to UserInteractive.
#[test]
fn raw_relay_bash_secret_handoff_survives_a_command_ahead_race() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let secret_command = r#"echo installed --api-key "sk-test-race-2142""#;
    let request = ShellHandoffRequest::new(
        secret_command,
        format!("$ {secret_command}"),
        "approved_provider_shell_tool",
        "user",
        "approval-race",
        "run-race",
        1,
    )
    .expect("handoff request");
    let token = request.token.clone();

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-handoff-race-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-handoff-race-test", &work_dir);
    config.native_mode = false;
    let mut pending = Some(request);
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(1_500)),
            RawRelayAction::line("sleep 1"),
            RawRelayAction::wait(Duration::from_millis(4_000)),
            RawRelayAction::line("echo after-race"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        move |events, _| {
            // Stage the handoff only while the unrelated command is running,
            // so its precmd fires between staging and the handoff line.
            let sleep_started = events.iter().any(|event| {
                event.kind == ShellEventKind::CommandStarted
                    && event.command.as_deref() == Some("sleep 1")
            });
            if !sleep_started {
                return Ok(RawObserverAction::Continue);
            }
            match pending.take() {
                Some(request) => Ok(RawObserverAction::EmitToPty(request)),
                None => Ok(RawObserverAction::Continue),
            }
        },
    )
    .expect("raw relay bash handoff race");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("after-race"), "{terminal}");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.origin == cosh_shell::types::CommandOrigin::ProviderTool)
        .unwrap_or_else(|| {
            panic!(
                "secret handoff block must keep ProviderTool despite the race; blocks={:?} terminal={terminal}",
                ledger
                    .blocks
                    .iter()
                    .map(|block| (block.command.clone(), block.origin))
                    .collect::<Vec<_>>()
            )
        });
    // Redaction still applies to the durable text; the token carried the claim.
    assert_eq!(block.command, "<redacted sensitive command>", "{terminal}");
    assert_eq!(
        block
            .audit_identity
            .as_ref()
            .and_then(|audit| audit.handoff_token.as_deref()),
        Some(token.as_str()),
        "{terminal}"
    );
}

/// zsh mirror of the command-ahead race above.
#[test]
fn raw_relay_zsh_secret_handoff_survives_a_command_ahead_race() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let secret_command = r#"echo installed --api-key "sk-test-race-2142""#;
    let request = ShellHandoffRequest::new(
        secret_command,
        format!("$ {secret_command}"),
        "approved_provider_shell_tool",
        "user",
        "approval-race",
        "run-race",
        1,
    )
    .expect("handoff request");
    let token = request.token.clone();

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-handoff-race-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-handoff-race-test", &work_dir);
    config.native_mode = false;
    let input = DelayedInput::new(vec![
        (b"sleep 1\n".to_vec(), Duration::from_millis(1_500)),
        (b"echo after-race\n".to_vec(), Duration::from_millis(5_500)),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let mut pending = Some(request);
    let output =
        run_raw_relay_zsh_with_output_control(&config, input, Vec::new(), move |events, _| {
            let sleep_started = events.iter().any(|event| {
                event.kind == ShellEventKind::CommandStarted
                    && event.command.as_deref() == Some("sleep 1")
            });
            if !sleep_started {
                return Ok(RawObserverAction::Continue);
            }
            match pending.take() {
                Some(request) => Ok(RawObserverAction::EmitToPty(request)),
                None => Ok(RawObserverAction::Continue),
            }
        })
        .expect("raw relay zsh handoff race");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("after-race"), "{terminal}");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.origin == cosh_shell::types::CommandOrigin::ProviderTool)
        .unwrap_or_else(|| {
            panic!(
                "secret handoff block must keep ProviderTool despite the race; blocks={:?} terminal={terminal}",
                ledger
                    .blocks
                    .iter()
                    .map(|block| (block.command.clone(), block.origin))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(block.command, "<redacted sensitive command>", "{terminal}");
    assert_eq!(
        block
            .audit_identity
            .as_ref()
            .and_then(|audit| audit.handoff_token.as_deref()),
        Some(token.as_str()),
        "{terminal}"
    );
}

/// #2142 review R3 (kongche P1 / SunnyQjm P1, timing (a)): the queued line
/// racing ahead is itself a user-typed bypass-prefixed command. Its wrapper
/// preexec must not adopt handoff treatment (active flag, pager policy,
/// token), and — crucially — its precmd must not see the active flag and
/// clear the request/token sidecars staged mid-execution; the real secret
/// handoff afterwards still claims by token.
#[test]
fn raw_relay_bash_secret_handoff_survives_a_bypass_prefixed_race() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let secret_command = r#"echo installed --api-key "sk-test-race-2142""#;
    let request = ShellHandoffRequest::new(
        secret_command,
        format!("$ {secret_command}"),
        "approved_provider_shell_tool",
        "user",
        "approval-bypass-race",
        "run-bypass-race",
        1,
    )
    .expect("handoff request");
    let token = request.token.clone();

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-handoff-bypass-race-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("bash-handoff-bypass-race-test", &work_dir);
    config.native_mode = false;
    let mut pending = Some(request);
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(1_500)),
            RawRelayAction::line("COSH_SHELL_HANDOFF_BYPASS=1 sleep 1"),
            RawRelayAction::wait(Duration::from_millis(4_000)),
            RawRelayAction::line("echo after-bypass-race"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
        move |events, _| {
            // The wrapper branch unwraps the bypass prefix, so the marker
            // reports the inner command text; stage the handoff while that
            // unrelated wrapper line is still running.
            let sleep_started = events.iter().any(|event| {
                event.kind == ShellEventKind::CommandStarted
                    && event.command.as_deref() == Some("sleep 1")
            });
            if !sleep_started {
                return Ok(RawObserverAction::Continue);
            }
            match pending.take() {
                Some(request) => Ok(RawObserverAction::EmitToPty(request)),
                None => Ok(RawObserverAction::Continue),
            }
        },
    )
    .expect("raw relay bash bypass race");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("after-bypass-race"), "{terminal}");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.origin == cosh_shell::types::CommandOrigin::ProviderTool)
        .unwrap_or_else(|| {
            panic!(
                "secret handoff block must keep ProviderTool despite the bypass-prefixed race; blocks={:?} terminal={terminal}",
                ledger
                    .blocks
                    .iter()
                    .map(|block| (block.command.clone(), block.origin))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(block.command, "<redacted sensitive command>", "{terminal}");
    assert_eq!(
        block
            .audit_identity
            .as_ref()
            .and_then(|audit| audit.handoff_token.as_deref()),
        Some(token.as_str()),
        "{terminal}"
    );
}

/// zsh mirror of the bypass-prefixed race above.
#[test]
fn raw_relay_zsh_secret_handoff_survives_a_bypass_prefixed_race() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let secret_command = r#"echo installed --api-key "sk-test-race-2142""#;
    let request = ShellHandoffRequest::new(
        secret_command,
        format!("$ {secret_command}"),
        "approved_provider_shell_tool",
        "user",
        "approval-bypass-race",
        "run-bypass-race",
        1,
    )
    .expect("handoff request");
    let token = request.token.clone();

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-handoff-bypass-race-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("zsh-handoff-bypass-race-test", &work_dir);
    config.native_mode = false;
    let input = DelayedInput::new(vec![
        (
            b"COSH_SHELL_HANDOFF_BYPASS=1 sleep 1\n".to_vec(),
            Duration::from_millis(1_500),
        ),
        (
            b"echo after-bypass-race\n".to_vec(),
            Duration::from_millis(5_500),
        ),
        (b"exit\n".to_vec(), Duration::from_millis(700)),
    ]);
    let mut pending = Some(request);
    let output =
        run_raw_relay_zsh_with_output_control(&config, input, Vec::new(), move |events, _| {
            let sleep_started = events.iter().any(|event| {
                event.kind == ShellEventKind::CommandStarted
                    && event.command.as_deref() == Some("sleep 1")
            });
            if !sleep_started {
                return Ok(RawObserverAction::Continue);
            }
            match pending.take() {
                Some(request) => Ok(RawObserverAction::EmitToPty(request)),
                None => Ok(RawObserverAction::Continue),
            }
        })
        .expect("raw relay zsh bypass race");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("after-bypass-race"), "{terminal}");

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.origin == cosh_shell::types::CommandOrigin::ProviderTool)
        .unwrap_or_else(|| {
            panic!(
                "secret handoff block must keep ProviderTool despite the bypass-prefixed race; blocks={:?} terminal={terminal}",
                ledger
                    .blocks
                    .iter()
                    .map(|block| (block.command.clone(), block.origin))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(block.command, "<redacted sensitive command>", "{terminal}");
    assert_eq!(
        block
            .audit_identity
            .as_ref()
            .and_then(|audit| audit.handoff_token.as_deref()),
        Some(token.as_str()),
        "{terminal}"
    );
}

/// #2196 review regression: `ShellHostConfig::new()` leaves the hint card
/// renderer unset (fail-quiet), so the public raw relay surface must offer
/// a working opt-in path. Wires a marker frame through the public
/// `set_hint_card_renderer` and drives an agent handoff into a blocked tty
/// read via the public bash entry point; the marker proves the sentinel
/// reached the injected renderer. The card is relay-injected into the
/// output sink (never echoed back by the PTY), so the assertion reads the
/// sink instead of `terminal_output`. Linux-only: the blocked-read
/// classification needs /proc evidence.
#[cfg(target_os = "linux")]
#[test]
fn raw_relay_bash_public_config_renders_hint_card_for_waiting_handoff() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("sink lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-public-hint-card-work-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("public-hint-card-test", &work_dir);
    config.native_mode = false;
    config.set_hint_card_renderer(|title, body| {
        let mut lines = vec![format!("[public-frame] {title}")];
        lines.extend(body);
        lines
    });

    // A forked child (`bash -c`) keeps the read outside the relay shell's
    // own process group: the sentinel requires a foreign foreground pgid
    // before it consults the /proc blocked-read evidence, and a bare
    // `read` builtin would run inside the shell itself and never qualify.
    let command = r#"bash -c 'read -p "Type y or n: " answer; echo "answer=$answer"'"#;
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            // The sentinel needs a 2s output-quiet window plus a 1s sample
            // interval before it may emit; the margin covers loaded runners.
            RawRelayAction::wait(Duration::from_millis(6_000)),
            RawRelayAction::line("y"),
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line("exit"),
        ],
        SharedSink(sink.clone()),
        emit_pager_policy_handoff(command, ImplicitPagerPolicy::Inherit),
    )
    .expect("raw relay bash public hint card handoff");

    let relayed = String::from_utf8_lossy(&sink.lock().expect("sink lock")).into_owned();
    assert!(
        relayed.contains("[public-frame]"),
        "hint card must reach the renderer injected through the public config path: {relayed}"
    );
    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(terminal.contains("answer=y"), "{terminal}");
}
