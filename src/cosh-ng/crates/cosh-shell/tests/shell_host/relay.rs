use super::*;

fn xtrace_record_contains_secret(rendered: &str, trace_prefix: &str, secret: &str) -> bool {
    rendered
        .split(['\r', '\n'])
        .any(|record| record.contains(trace_prefix) && record.contains(secret))
}

fn exact_screen_text_occurrences(screen: &str, expected: &str) -> usize {
    screen
        .match_indices(expected)
        .filter(|(start, _)| {
            screen
                .as_bytes()
                .get(start + expected.len())
                .is_none_or(|next| !next.is_ascii_alphanumeric())
        })
        .count()
}

#[test]
fn xtrace_secret_scan_treats_cr_as_a_record_boundary() {
    let separate_echo = " /auth SECRET\r...\rxxtrace-guard: command\r\n";
    let traced_secret = " /auth SECRET\r...\rxxtrace-guard: command SECRET\r\n";

    assert!(!xtrace_record_contains_secret(
        separate_echo,
        "xtrace-guard:",
        "SECRET"
    ));
    assert!(xtrace_record_contains_secret(
        traced_secret,
        "xtrace-guard:",
        "SECRET"
    ));
}

#[test]
fn terminal_screen_redraw_erases_typed_line_and_cursor_ghost() {
    let output = concat!(
        "[root@host ~]# /mode",
        "\x1b7\x1b[2m hint\x1b[0m\x1b8",
        "\r\x1b[K",
        "[root@host ~]# builtin true __cosh_slash_guard__\r\n"
    );

    let screen = render_terminal_screen(output.as_bytes(), 200, 50);
    assert_eq!(
        screen
            .iter()
            .filter(|line| line.contains("]# /mode"))
            .count(),
        0
    );
    assert_eq!(
        screen.first().map(String::as_str),
        Some("[root@host ~]# builtin true __cosh_slash_guard__")
    );

    let scrolled = render_terminal_screen(b"first\r\nsecond\r\nthird", 20, 2);
    assert_eq!(scrolled, ["second", "third"]);

    let tabbed = render_terminal_screen(b"a\tb", 20, 2);
    assert_eq!(tabbed.first().map(String::as_str), Some("a       b"));
}

#[test]
fn raw_relay_bash_renders_direct_slash_submission_exactly_once() {
    assert_bash_slash_screen(
        "bash-direct-slash-screen",
        "/mode",
        200,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/mode"),
            RawRelayAction::wait(Duration::from_millis(600)),
        ],
    );
}

#[test]
fn raw_relay_bash_renders_recalled_slash_submission_exactly_once() {
    assert_bash_slash_screen(
        "bash-recalled-slash-screen",
        "/mode",
        200,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(600)),
        ],
    );
}

#[test]
fn raw_relay_bash_renders_wrapped_ascii_slash_submission_exactly_once() {
    let command = "/mode aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_bash_slash_screen(
        "bash-wrapped-ascii-slash-screen",
        command,
        40,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(command),
            RawRelayAction::wait(Duration::from_millis(600)),
        ],
    );
}

fn assert_bash_slash_screen(
    test_id: &str,
    command: &str,
    width: u16,
    mut actions: Vec<RawRelayAction>,
) {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-{test_id}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         shopt -s histappend\n\
         PS1='[root@host ~]# '\n",
    )
    .expect("bashrc");
    std::fs::write(home.join(".bash_history"), format!("{command}\n")).expect("history");

    let mut config =
        ShellHostConfig::new(test_id, &work_dir).with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    config.winsize.ws_col = width;
    config.winsize.ws_row = 50;
    let mut rendered = Vec::new();
    actions.push(RawRelayAction::line("exit"));
    let output = run_raw_relay_bash_with_actions(&config, actions, &mut rendered)
        .expect("slash screen relay");

    // The enhanced shell adds a UTF-8 ownership glyph outside PS1. Remove
    // that decoration so this oracle exercises only ASCII column semantics.
    let ascii_rendered: Vec<u8> = rendered.iter().copied().filter(u8::is_ascii).collect();
    let screen = render_terminal_screen(&ascii_rendered, usize::from(width), 50);
    assert_eq!(
        exact_screen_text_occurrences(&screen.concat(), command),
        1,
        "screen: {screen:#?}"
    );
    assert!(
        screen
            .iter()
            .all(|line| !line.contains("__cosh_slash_guard__")),
        "screen: {screen:#?}"
    );
    assert!(
        !rendered
            .windows(b"__cosh_slash_guard__".len())
            .any(|window| window == b"__cosh_slash_guard__"),
        "raw terminal bytes leaked the internal slash guard"
    );
    assert!(
        !rendered
            .windows(b"in set -x ;; *) : ;; esac".len())
            .any(|window| window == b"in set -x ;; *) : ;; esac"),
        "raw terminal bytes leaked the horizontally scrolled slash guard"
    );
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some(command)
            && event.component.as_deref() == Some("slash")
    }));
    let history = std::fs::read_to_string(home.join(".bash_history")).expect("history");
    assert!(history.lines().any(|line| line == command), "{history}");
    assert!(!history.contains("__cosh_slash_guard__"), "{history}");

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_invalid_utf8_never_enters_event_provenance() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-invalid-utf8-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = with_raw_byte_readline(ShellHostConfig::new("invalid-utf8-test", &work_dir));
    config.native_mode = false;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(
        &config,
        std::io::Cursor::new(vec![0xff, b'\n', b'e', b'x', b'i', b't', b'\n']),
        &mut rendered,
    )
    .expect("invalid utf8 relay");

    assert!(!format!("{:?}", output.events).contains('\u{fffd}'));
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event
                .command
                .as_deref()
                .is_some_and(|command| command != "exit")
    }));
    let routing = output
        .events
        .iter()
        .find(|event| event.kind == ShellEventKind::CommandRoutingObserved)
        .unwrap_or_else(|| panic!("missing raw-free routing evidence: {:?}", output.events));
    assert!(routing.command.is_none() && routing.input.is_none());
    assert!(routing.routing.as_ref().is_some_and(|metadata| {
        metadata.unsafe_input && !metadata.proven && metadata.top_level_missing
    }));
}

#[test]
fn routing_c4_zsh_rust_route_uses_shared_event_contract() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-raw-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let unicode_file = work_dir.join("\u{8bbe}\u{8ba1}\u{6587}\u{6863}.md");
    std::fs::write(&unicode_file, "\u{4e2d}\u{6587}\u{5185}\u{5bb9}").expect("unicode file");

    let config = ShellHostConfig::new("zsh-raw-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::line("/help"),
            RawRelayAction::line("echo zsh-raw-ok"),
            RawRelayAction::line(format!("cat {}", shell_arg(&unicode_file))),
            RawRelayAction::line("ls /path/that/does/not/exist"),
        ],
        &mut rendered,
    )
    .expect("raw zsh relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("zsh-raw-ok"), "{rendered_text}");
    assert!(
        rendered_text.contains("\u{4e2d}\u{6587}\u{5185}\u{5bb9}"),
        "{rendered_text}"
    );
    assert_no_osc_marker(&rendered);
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/help")
            && event.component.as_deref() == Some("slash")
    }));

    let ledger = ledger_from_output(&output);
    let echo_block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("echo zsh-raw-ok") && block.exit_code == 0)
        .expect("zsh echo command block");
    assert_clean_shell_output_ref(echo_block, "zsh-raw-ok");
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("cat ") && block.exit_code == 0));
    assert!(ledger.blocks.iter().any(|block| {
        block.command.contains("/path/that/does/not/exist") && block.exit_code != 0
    }));
}

#[test]
fn removed_draft_alias_preserves_native_shell_routing() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c4-draft-grammar-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c4-draft-grammar", &work_dir);
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("/draft extra"),
            RawRelayAction::line("/draft 'extra'"),
        ],
        Vec::new(),
    )
    .expect("removed draft routing");

    for input in ["/draft extra", "/draft 'extra'"] {
        assert!(!output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some(input)
                && event.component.as_deref() == Some("slash")
        }));
        assert!(output.events.iter().any(|event| {
            event.kind == ShellEventKind::CommandStarted && event.command.as_deref() == Some(input)
        }));
    }
}

#[test]
fn raw_relay_zsh_buffers_fragmented_intercept_candidates() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-fragment-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    let config = ShellHostConfig::new("zsh-fragment-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::write("/he"),
            RawRelayAction::write("lp\n"),
            RawRelayAction::write("\u{4f60}".as_bytes()),
            RawRelayAction::write("\u{597d}\n".as_bytes()),
            RawRelayAction::write("?? zsh "),
            RawRelayAction::write("fragmented agent\n"),
            RawRelayAction::write("?? zsh combined agent\necho after-zsh-combined\n"),
            RawRelayAction::line("echo after-zsh-fragment"),
        ],
        &mut rendered,
    )
    .expect("raw zsh fragmented relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        rendered_text.contains("after-zsh-fragment"),
        "{rendered_text}"
    );
    assert!(
        rendered_text.contains("after-zsh-combined"),
        "{rendered_text}"
    );
    assert!(!rendered_text.contains("zsh: no such file or directory: /help"));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/help")
            && event.component.as_deref() == Some("slash")
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("\u{4f60}\u{597d}")
            && event.component.as_deref() == Some("natural_language")
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("?? zsh fragmented agent")
            && event.component.as_deref() == Some("agent_marker")
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("?? zsh combined agent")
            && event.component.as_deref() == Some("agent_marker")
    }));
}

#[test]
fn routing_c3_valid_slash_intercepts_fragmented_input() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-slash-completion-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    let config = ShellHostConfig::new("slash-completion-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::write(b"/".to_vec()),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::write(b"mo".to_vec()),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::write(b"de approval auto\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
        |_, _| Ok(RawObserverAction::Continue),
    )
    .expect("raw bash slash completion");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("/"), "{rendered_text}");
    assert!(
        !rendered_text.contains("cosh-osc$ /  /help  /mode  /details  /skill"),
        "{rendered_text}"
    );
    assert!(!rendered_text.contains("/m/mo/mod/mode"), "{rendered_text}");
    assert!(
        output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("/mode approval auto")
                && event.component.as_deref() == Some("slash")
        }),
        "{rendered_text}\n{:?}",
        output.events
    );
    assert!(!rendered_text.contains("bash: /mode"), "{rendered_text}");
}

#[test]
fn raw_relay_bash_up_recalls_intercepted_slash_command() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-1718-recall-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         shopt -s histappend\n\
         PS1='guard$ '\n",
    )
    .expect("bashrc");
    std::fs::write(home.join(".bash_history"), "echo prior-shell-cmd\n").expect("history");

    let mut config = ShellHostConfig::new("bash-1718-recall", &work_dir)
        .with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/skills detail xlsx"),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("1718 recall relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    let intercept_count = output
        .events
        .iter()
        .filter(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("/skills detail xlsx")
                && event.component.as_deref() == Some("slash")
        })
        .count();
    let recalled_prior_shell_cmd = output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event.command.as_deref() == Some("echo prior-shell-cmd")
    });
    // The Readline guard records the first submission and intercepts both it
    // and the recalled copy before Bash parses either command.
    assert_eq!(intercept_count, 2, "{rendered_text}");
    assert!(!recalled_prior_shell_cmd, "{rendered_text}");
    // The routed line must never execute as a shell command.
    assert!(!rendered_text.contains("bash: /skills"), "{rendered_text}");
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "the internal slash guard must never reach the terminal: {rendered_text}"
    );
    assert!(
        rendered_text.contains("/skills detail xlsx"),
        "the original slash line must remain visible: {rendered_text}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_vi_mode_guards_slash_without_mutating_commands() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-vi-guard-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join(".bashrc"), "set -o vi\n").expect("bashrc");

    let mut config = ShellHostConfig::new("bash-vi-guard", &work_dir)
        .with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    config.raw_action_watchdog = Duration::from_secs(10);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/skills detail xlsx"),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::line("echo probe-ordinary-tail"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("vi-mode slash guard relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/skills detail xlsx")
            && event.component.as_deref() == Some("slash")
    }));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event.command.as_deref() == Some("echo probe-ordinary-tail")
    }));
    assert!(
        rendered_text.contains("probe-ordinary-tail"),
        "{rendered_text}"
    );
    assert!(
        !rendered_text.contains("probe-ordinary-taiL"),
        "{rendered_text}"
    );
    assert!(!rendered_text.contains("bash: /skills"), "{rendered_text}");
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "the internal slash guard must never reach the vi terminal: {rendered_text}"
    );
    assert!(!rendered_text.contains("bash: exiT"), "{rendered_text}");

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_xtrace_hides_guard_protocol_and_sentinel() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-xtrace-guard-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         shopt -s histappend\n\
         PS4='xtrace-guard: '; set -x\n",
    )
    .expect("bashrc");

    let mut config = ShellHostConfig::new("bash-xtrace-guard", &work_dir)
        .with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let secret = "sk-test-private-2942";
    let private_auth = format!(" /auth {secret}");
    let xtrace_probe = "case $- in *x*) probe=ON ;; *) probe=OFF ;; esac; \
                        printf '__XTRACE_%s__\\n' \"$probe\"; unset probe";
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(private_auth),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::line("/mode"),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::line(xtrace_probe),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("xtrace slash guard relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/mode")
            && event.component.as_deref() == Some("slash")
    }));
    assert!(
        output.events.iter().any(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.component.as_deref() == Some("slash")
                && event.input.as_deref() == Some("<redacted>")
                && event
                    .routing
                    .as_ref()
                    .is_some_and(|routing| routing.sensitive)
        }),
        "events: {:#?}\nrendered: {rendered_text}",
        output.events
    );
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event.command.as_deref() == Some(xtrace_probe)
    }));
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "xtrace leaked the internal sentinel: {rendered_text}"
    );
    assert!(
        !rendered_text.contains("1337;COSH;"),
        "xtrace leaked the authenticated marker protocol: {rendered_text}"
    );
    assert!(
        !xtrace_record_contains_secret(&rendered_text, "xtrace-guard:", secret),
        "xtrace leaked the private slash input: {rendered_text}"
    );
    assert!(
        rendered_text.contains("__XTRACE_ON__"),
        "slash guard did not preserve xtrace: {rendered_text}"
    );
    assert!(
        !rendered_text.contains("__XTRACE_OFF__"),
        "slash guard disabled the user's xtrace state: {rendered_text}"
    );
    let history = std::fs::read_to_string(home.join(".bash_history")).unwrap_or_default();
    assert!(
        !history.contains(secret),
        "private history leaked: {history}"
    );
    let journal = std::fs::read_to_string(&output.journal_path).expect("journal");
    assert!(
        !journal.contains(secret),
        "journal leaked private slash input"
    );
    let evidence = ledger_output_refs_text(&ledger_from_output(&output));
    assert!(
        !evidence.contains(secret),
        "evidence leaked private slash input"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

fn assert_bash_guard_preserves_partial_debug_output(
    test_id: &str,
    trap_output: &str,
    expected_output: &str,
) {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-{test_id}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join(".bashrc"), "PS1='guard$ '\n").expect("bashrc");

    let mut config =
        ShellHostConfig::new(test_id, &work_dir).with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let trap_setup = format!(
        "set -T; trap 'case \"$BASH_COMMAND\" in READLINE_LINE=*) \
         {trap_output} > /dev/tty;; esac' DEBUG"
    );
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(&trap_setup),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/mode"),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::line("trap - DEBUG"),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("partial-output slash guard relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/mode")
            && event.component.as_deref() == Some("slash")
    }));
    assert!(
        rendered_text.contains(expected_output),
        "the guard filter dropped unrelated partial output: {rendered_text}"
    );
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "the internal sentinel reached the terminal: {rendered_text}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_preserves_partial_debug_output_before_guard_shadow() {
    assert_bash_guard_preserves_partial_debug_output(
        "partial-guard",
        "printf \"BACKGROUND_%s\" PARTIAL",
        "BACKGROUND_PARTIAL",
    );
}

#[test]
fn raw_relay_bash_preserves_carriage_return_debug_output_before_guard_shadow() {
    assert_bash_guard_preserves_partial_debug_output(
        "carriage-partial-guard",
        "printf \"BEFORE\\rBACKGROUND_%s\" PARTIAL",
        "BACKGROUND_PARTIAL",
    );
}

#[test]
fn raw_relay_bash_verbose_mode_hides_both_guard_echoes() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-verbose-guard-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join(".bashrc"), "PS1='guard$ '\n").expect("bashrc");

    let mut config = ShellHostConfig::new("bash-verbose-guard", &work_dir)
        .with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("set -v"),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/mode"),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::line("set +v"),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("verbose slash guard relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("/mode")
            && event.component.as_deref() == Some("slash")
    }));
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "verbose mode leaked the internal sentinel: {rendered_text}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn routing_c4_bash_route_enters_native_history_file() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-1718-histfile-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    let expansion_side_effect = root.join("slash-expanded");
    let slash_input = format!(
        "/skills detail $(touch {})",
        expansion_side_effect.display()
    );
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         shopt -s histappend\n",
    )
    .expect("bashrc");

    let mut config = ShellHostConfig::new("bash-1718-histfile", &work_dir)
        .with_env("HOME", home.display().to_string())
        // A user's colon-no-op filter must not discard Cosh's replacement
        // placeholder before the original slash line is committed.
        .with_env("HISTIGNORE", "\\:*");
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(&slash_input),
            RawRelayAction::wait(Duration::from_millis(1200)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("1718 histfile relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    let intercepted = output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some(slash_input.as_str())
            && event.component.as_deref() == Some("slash")
    });
    assert!(intercepted, "{rendered_text}");
    // Bash owns persistence after the Readline guard records the slash line.
    let history = std::fs::read_to_string(home.join(".bash_history")).expect("histfile");
    assert!(history.contains(&slash_input), "{history}");
    assert!(!history.contains("__cosh_slash_guard__"), "{history}");
    assert!(
        !expansion_side_effect.exists(),
        "slash input reached Bash expansion"
    );
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "a slash longer than the guard must not leak the guard: {rendered_text}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_guards_each_slash_in_shell_owned_batch() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-batch-guard-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let work_dir = root.join("work");
    let expansion_side_effect = root.join("batch-slash-expanded");
    let first = "/skills detail first";
    let second = format!(
        "/skills detail $(touch {})",
        expansion_side_effect.display()
    );

    let mut config = ShellHostConfig::new("bash-batch-slash-guard", &work_dir);
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::write(format!("{first}\n{second}\n").into_bytes()),
            RawRelayAction::wait(Duration::from_millis(1200)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("same-read slash batch relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    for input in [first, second.as_str()] {
        assert!(
            output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.input.as_deref() == Some(input)
                    && event.component.as_deref() == Some("slash")
            }),
            "missing slash intercept for {input}: {rendered_text}"
        );
    }
    assert!(
        !expansion_side_effect.exists(),
        "batched slash reached Bash expansion"
    );
    assert!(!rendered_text.contains("bash: /skills"), "{rendered_text}");
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "same-write slash guards leaked internal redisplay: {rendered_text}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_preserves_same_read_foreground_stdin_bytes() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-foreground-stdin-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let work_dir = root.join("work");
    let captured = root.join("captured-stdin");
    let payload = b"stdin-probe\x1b[Dliteral";
    let command = format!(
        "sh -c 'IFS= read -r value; printf \"%s\" \"$value\" > \"$1\"' sh {}",
        shell_arg(&captured)
    );

    let mut config = ShellHostConfig::new("bash-foreground-stdin", &work_dir);
    config.slash_via_shell = true;
    config.raw_action_watchdog = Duration::from_secs(10);
    let mut rendered = Vec::new();
    let mut batch = command.into_bytes();
    batch.push(b'\n');
    batch.extend_from_slice(payload);
    batch.push(b'\n');
    run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::write(batch),
            RawRelayAction::wait(Duration::from_millis(1200)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("same-read foreground stdin relay");

    let captured_bytes = std::fs::read(&captured).expect("captured foreground stdin");
    assert_eq!(
        captured_bytes, payload,
        "foreground stdin must not receive a Readline guard"
    );
    assert!(
        !captured_bytes
            .windows(b"\x1b[99~".len())
            .any(|window| window == b"\x1b[99~"),
        "foreground stdin contains a Readline guard: {captured_bytes:?}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_shell_opaque_bytes_ignore_slash_route() {
    struct ForegroundSynchronizedInput {
        batch: Option<Vec<u8>>,
        foreground_done: std::path::PathBuf,
        exit_sent: bool,
        deadline: Instant,
    }

    impl Read for ForegroundSynchronizedInput {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if let Some(batch) = self.batch.take() {
                std::thread::sleep(Duration::from_millis(200));
                assert!(batch.len() <= buf.len());
                buf[..batch.len()].copy_from_slice(&batch);
                return Ok(batch.len());
            }
            if !self.exit_sent {
                while std::fs::metadata(&self.foreground_done)
                    .map(|metadata| metadata.len() == 0)
                    .unwrap_or(true)
                {
                    if Instant::now() >= self.deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "foreground reader did not complete",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                self.exit_sent = true;
                buf[..b"exit\n".len()].copy_from_slice(b"exit\n");
                return Ok(b"exit\n".len());
            }
            Ok(0)
        }
    }

    fn run(slash_via_shell: bool) {
        let root = std::env::temp_dir().join(format!(
            "cosh-shell-bash-shell-opaque-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let work_dir = root.join("work");
        let captured = root.join("captured-stdin");
        let command = format!(
            "/bin/sh -c 'IFS= read -r first; IFS= read -r second; \
             printf \"%s\\n%s\" \"$first\" \"$second\" > \"$1\"' sh {}",
            shell_arg(&captured)
        );

        let mut config = ShellHostConfig::new("bash-shell-opaque", &work_dir);
        config.slash_via_shell = slash_via_shell;
        config.raw_action_watchdog = Duration::from_secs(10);
        let mut rendered = Vec::new();
        let mut batch = command.into_bytes();
        batch.extend_from_slice(b"\n/help\nstdin-probe\nforeground-second\n");
        let input = ForegroundSynchronizedInput {
            batch: Some(batch),
            foreground_done: captured.clone(),
            exit_sent: false,
            deadline: Instant::now() + Duration::from_secs(10),
        };
        run_raw_relay_bash(&config, input, &mut rendered).expect("same-read shell-opaque relay");

        let captured_bytes = std::fs::read(&captured).expect("captured foreground stdin");
        assert!(!captured_bytes.contains(&0x1b), "{captured_bytes:?}");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    run(true);
    run(false);
}

#[test]
fn raw_relay_bash_slash_route_switch_off_keeps_rust_intercept() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-1718-switch-off-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         shopt -s histappend\n",
    )
    .expect("bashrc");
    std::fs::write(home.join(".bash_history"), "echo prior-shell-cmd\n").expect("history");

    let mut config = ShellHostConfig::new("bash-1718-switch-off", &work_dir)
        .with_env("HOME", home.display().to_string());
    config.slash_via_shell = false;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/skills detail xlsx"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("1718 switch-off relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    let intercept_count = output
        .events
        .iter()
        .filter(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("/skills detail xlsx")
                && event.component.as_deref() == Some("slash")
        })
        .count();
    let recalled_prior_shell_cmd = output.events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            && event.command.as_deref() == Some("echo prior-shell-cmd")
    });
    // The compatibility switch keeps slash controls on the Rust-owned path.
    assert_eq!(intercept_count, 1, "{rendered_text}");
    assert!(recalled_prior_shell_cmd, "{rendered_text}");

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn routing_c4_history_privacy_secret_slash_never_persists() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-1718-secret-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         shopt -s histappend\n",
    )
    .expect("bashrc");

    let mut config = ShellHostConfig::new("bash-1718-secret", &work_dir)
        .with_env("HOME", home.display().to_string());
    config.slash_via_shell = true;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("/config set api_key=sk-test-secret"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("1718 secret relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    let intercepted = output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("slash")
    });
    assert!(intercepted, "{rendered_text}");
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "sensitive slash submission leaked the guard: {rendered_text}"
    );
    // The Readline guard recognizes the secret and omits it from history.
    let history = std::fs::read_to_string(home.join(".bash_history")).unwrap_or_default();
    assert!(!history.contains("api_key"), "{history}");

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_intercepts_history_recalled_slash_with_enter_and_ctrl_o() {
    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-recalled-slash-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         export HISTCONTROL=ignoredups\n\
         shopt -s histappend\n",
    )
    .expect("bashrc");
    std::fs::write(home.join(".bash_history"), "/skills detail xlsx\n").expect("history");

    let config = ShellHostConfig::new("bash-recalled-slash-test", &work_dir)
        .with_env("HOME", home.display().to_string());
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            // `operate-and-get-next` accepts the recalled Readline buffer
            // without carrying CR/LF in the user input batch.
            RawRelayAction::write(b"\x0f".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("recalled slash relay");

    let rendered_text = String::from_utf8_lossy(&rendered);
    let intercept_count = output
        .events
        .iter()
        .filter(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.input.as_deref() == Some("/skills detail xlsx")
                && event.component.as_deref() == Some("slash")
        })
        .count();
    assert_eq!(intercept_count, 2, "{rendered_text}");
    assert!(
        !rendered_text.contains("bash: /skills: No such file or directory"),
        "{rendered_text}"
    );
    assert!(
        !rendered_text.contains("__cosh_slash_guard__"),
        "the internal slash guard must never reach Ctrl-O or Enter output: {rendered_text}"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_bash_routes_recalled_and_indented_natural_language() {
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let root = std::env::temp_dir().join(format!(
        "cosh-shell-bash-history-recall-2951-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let home = root.join("home");
    let work_dir = root.join("work");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join(".bashrc"),
        "export HISTFILE=\"$HOME/.bash_history\"\n\
         export HISTSIZE=1000\n\
         export HISTFILESIZE=1000\n\
         shopt -s histappend\n",
    )
    .expect("bashrc");

    let prompt = "你好你是谁";
    let secret = "TEST_ONLY_SECRET_2951";
    let control = home.join(".history-recall-control");
    let indented_control = home.join(".history-recall-indented-control");
    let config = ShellHostConfig::new("bash-history-recall-2951", &work_dir)
        .with_env("HOME", home.display().to_string());
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(prompt),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(b"\x1b[A".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"?\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line(" 你好"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("   你好"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write("\t你好\n".as_bytes().to_vec()),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line(format!("printf ignored > /dev/null # token={secret}")),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("printf x > \"$HOME/.history-recall-control\""),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("  printf y > \"$HOME/.history-recall-indented-control\""),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("history recall relay");

    let routed = output
        .events
        .iter()
        .filter(|event| {
            event.kind == ShellEventKind::UserInputIntercepted
                && event.component.as_deref() == Some("natural_language")
        })
        .map(|event| event.input.as_deref().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        routed,
        [prompt, prompt, "你好你是谁?", " 你好", "   你好", "你好"],
        "events: {:#?}\nrendered: {}",
        output.events,
        String::from_utf8_lossy(&rendered)
    );

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        !rendered_text.contains("command not found"),
        "{rendered_text}"
    );
    for internal in ["__cosh_slash_guard__", "_COSH_HANDOFF", "1337;COSH;"] {
        assert!(
            !rendered_text.contains(internal),
            "{internal}: {rendered_text}"
        );
    }
    assert_eq!(std::fs::read(&control).expect("shell control"), b"x");
    assert_eq!(
        std::fs::read(&indented_control).expect("indented shell control"),
        b"y"
    );

    let history = std::fs::read_to_string(home.join(".bash_history")).expect("history");
    assert_eq!(
        history.lines().filter(|line| *line == prompt).count(),
        1,
        "{history}"
    );
    assert_eq!(
        history
            .lines()
            .filter(|line| *line == "printf x > \"$HOME/.history-recall-control\"")
            .count(),
        1,
        "{history}"
    );
    assert!(!history.contains(secret), "{history}");
    let journal = std::fs::read_to_string(&output.journal_path).expect("journal");
    assert!(!journal.contains(secret), "{journal}");
    assert!(!ledger_output_refs_text(&ledger_from_output(&output)).contains(secret));

    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn raw_relay_zsh_preserves_session_history() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-history-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");

    let mut config = ShellHostConfig::new("zsh-history-test", &work_dir);
    config.native_mode = false;
    let mut rendered = Vec::new();
    run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::line("pwd"),
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::line("history"),
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::line("ls -ltrh"),
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::line("history"),
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
    )
    .expect("raw zsh history");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("    1  pwd"), "{rendered_text}");
    assert!(
        rendered_text.contains("    3  ls -ltrh") || rendered_text.contains("    2  ls -ltrh"),
        "{rendered_text}"
    );
}

#[test]
fn raw_relay_bash_excludes_secrets_from_history_and_journal() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-bash-secret-history-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let history_snapshot = work_dir.join("history-snapshot");
    let secret = "history-secret-value";
    let edited_secret = "history-edited-secret-value";
    let access_key = "LTAI5tExampleAccessKey";
    let url_password = "history-url-password";
    let empty_user_url_password = "history-empty-user-url-password";
    let dynamic_option_secret = "history-dynamic-option-secret";
    let nested_dynamic_option_secret = "history-nested-dynamic-option-secret";
    let mut config = ShellHostConfig::new("bash-secret-history-test", &work_dir);
    config.native_mode = false;
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(format!("TOKEN={secret} true")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(format!("TOKEN={edited_secret} true").into_bytes()),
            RawRelayAction::write(b"\x1b[D\x1b[C".to_vec()),
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(": {access_key}")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(": https://user:{url_password}@example.test")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(": http://:{empty_user_url_password}@example.test")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("HEADER_OPTION=-H"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": ${{HEADER_OPTION}}\"Cookie: session={dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": $(true; printf -- -H)\"Cookie: session={nested_dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": $(true && printf -- -H)\"Cookie: session={nested_dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": $(printf -- -H | cat)\"Cookie: session={nested_dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!("history > {}", shell_arg(&history_snapshot))),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
    )
    .expect("raw bash secret history");

    let history = std::fs::read_to_string(&history_snapshot).expect("history snapshot");
    let journal = std::fs::read_to_string(&output.journal_path).expect("journal");
    assert!(!history.contains(secret), "{history}");
    assert!(!history.contains(edited_secret), "{history}");
    assert!(!history.contains(access_key), "{history}");
    assert!(!history.contains(url_password), "{history}");
    assert!(!history.contains(empty_user_url_password), "{history}");
    assert!(!history.contains(dynamic_option_secret), "{history}");
    assert!(!history.contains(nested_dynamic_option_secret), "{history}");
    assert!(!journal.contains(secret), "{journal}");
    assert!(!journal.contains(edited_secret), "{journal}");
    assert!(!journal.contains(access_key), "{journal}");
    assert!(!journal.contains(url_password), "{journal}");
    assert!(!journal.contains(empty_user_url_password), "{journal}");
    assert!(!journal.contains(dynamic_option_secret), "{journal}");
    assert!(!journal.contains(nested_dynamic_option_secret), "{journal}");
    assert!(ledger_from_output(&output)
        .blocks
        .iter()
        .all(|block| !block.command.contains(secret)
            && !block.command.contains(edited_secret)
            && !block.command.contains(access_key)
            && !block.command.contains(url_password)
            && !block.command.contains(empty_user_url_password)
            && !block.command.contains(dynamic_option_secret)
            && !block.command.contains(nested_dynamic_option_secret)));
}

#[test]
fn raw_relay_zsh_excludes_secrets_from_history_and_journal() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-secret-history-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let history_snapshot = work_dir.join("history-snapshot");
    let secret = "history-secret-value";
    let access_key = "LTAI5tExampleAccessKey";
    let url_password = "history-url-password";
    let empty_user_url_password = "history-empty-user-url-password";
    let dynamic_option_secret = "history-dynamic-option-secret";
    let nested_dynamic_option_secret = "history-nested-dynamic-option-secret";
    let mut config = ShellHostConfig::new("zsh-secret-history-test", &work_dir);
    config.native_mode = false;
    let output = run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::line(format!("TOKEN={secret} true")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(": {access_key}")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(": https://user:{url_password}@example.test")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(": http://:{empty_user_url_password}@example.test")),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("HEADER_OPTION=-H"),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": ${{HEADER_OPTION}}\"Cookie: session={dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": $(true; printf -- -H)\"Cookie: session={nested_dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": $(true && printf -- -H)\"Cookie: session={nested_dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!(
                ": $(printf -- -H | cat)\"Cookie: session={nested_dynamic_option_secret}\""
            )),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line(format!("fc -l -100 > {}", shell_arg(&history_snapshot))),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("exit"),
        ],
        Vec::new(),
    )
    .expect("raw zsh secret history");

    let history = std::fs::read_to_string(&history_snapshot).expect("history snapshot");
    let journal = std::fs::read_to_string(&output.journal_path).expect("journal");
    assert!(!history.contains(secret), "{history}");
    assert!(!history.contains(access_key), "{history}");
    assert!(!history.contains(url_password), "{history}");
    assert!(!history.contains(empty_user_url_password), "{history}");
    assert!(!history.contains(dynamic_option_secret), "{history}");
    assert!(!history.contains(nested_dynamic_option_secret), "{history}");
    assert!(!journal.contains(secret), "{journal}");
    assert!(!journal.contains(access_key), "{journal}");
    assert!(!journal.contains(url_password), "{journal}");
    assert!(!journal.contains(empty_user_url_password), "{journal}");
    assert!(!journal.contains(dynamic_option_secret), "{journal}");
    assert!(!journal.contains(nested_dynamic_option_secret), "{journal}");
    assert!(ledger_from_output(&output)
        .blocks
        .iter()
        .all(|block| !block.command.contains(secret)
            && !block.command.contains(access_key)
            && !block.command.contains(url_password)
            && !block.command.contains(empty_user_url_password)
            && !block.command.contains(dynamic_option_secret)
            && !block.command.contains(nested_dynamic_option_secret)));
}

#[test]
fn raw_relay_hold_mode_drops_input_without_writing_to_bash() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-hold-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("hold-test", &work_dir);
    let mut observer_calls = 0usize;
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::line("echo should-not-run"),
        ],
        Vec::new(),
        move |_, _| {
            observer_calls += 1;
            if observer_calls < 20 {
                Ok(RawObserverAction::HoldShellOutput)
            } else {
                Ok(RawObserverAction::Continue)
            }
        },
    )
    .expect("raw relay hold mode");

    let terminal = String::from_utf8_lossy(&output.terminal_output);
    assert!(!terminal.contains("should-not-run"), "{terminal}");
    let ledger = ledger_from_output(&output);
    assert!(!ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("should-not-run")));
}

#[test]
fn raw_relay_hold_mode_still_observes_ctrl_c() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-hold-ctrl-c-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("hold-ctrl-c-test", &work_dir);
    let mut observer_calls = 0usize;
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(vec![0x03]),
        ],
        Vec::new(),
        move |_, _| {
            observer_calls += 1;
            if observer_calls < 20 {
                Ok(RawObserverAction::HoldShellOutput)
            } else {
                Ok(RawObserverAction::Continue)
            }
        },
    )
    .expect("raw relay hold ctrl-c");

    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("control")
            && event.input.as_deref() == Some("ctrl_c")
    }));
}

#[test]
fn raw_relay_capture_ack_replays_same_read_multiline_suffix() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-capture-drain-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("capture-drain-test", &work_dir);
    config.native_mode = false;
    let capture = RawInputCapture::Question {
        id: "question-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(b"yes\necho capture-drain-ok\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(400)),
        ],
        Vec::new(),
        move |events, _| {
            if events.iter().any(|event| {
                event.component.as_deref() == Some("card")
                    && event.message.as_deref() == Some("answer")
            }) {
                Ok(RawObserverAction::Continue)
            } else {
                Ok(RawObserverAction::CaptureInput(capture.clone()))
            }
        },
    )
    .expect("capture drain relay");

    // issue #1913: the suffix typed in the same read as the submitting
    // Enter is type-ahead, not capture input. A cleanly drained chain
    // replays it to the shell instead of silently discarding it.
    let blocks: Vec<_> = ledger_from_output(&output)
        .blocks
        .into_iter()
        .filter(|block| block.command == "echo capture-drain-ok")
        .collect();
    assert!(!blocks.is_empty(), "{:?}", output.events);
    assert!(output.events.iter().any(|event| {
        event.message.as_deref() == Some("capture_submitted")
            && event.capture.as_ref().is_some_and(|capture| {
                capture.kind.as_deref() == Some("question")
                    && capture.target_id.as_deref() == Some("question-1")
                    && capture.generation > 0
                    && capture.lifecycle == cosh_shell::types::ShellCaptureLifecycle::Submitted
            })
    }));
    assert!(output
        .events
        .iter()
        .any(|event| event.message.as_deref() == Some("capture_drained")));
    // The replayed suffix must not surface a rejection notice.
    assert!(!output
        .events
        .iter()
        .any(|event| event.message.as_deref() == Some("capture_input_rejected")));
}

#[test]
fn raw_relay_capture_chain_discards_old_generation_suffix() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-capture-chain-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("capture-chain-test", &work_dir);
    config.native_mode = false;
    let first = RawInputCapture::Question {
        id: "question-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let second = RawInputCapture::Question {
        id: "question-2".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(b"first\nsecond\necho capture-chain-ok\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(b"actual-second\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
        ],
        Vec::new(),
        move |events, _| {
            if events.iter().any(|event| {
                event.component.as_deref() == Some("card")
                    && event.message.as_deref() == Some("answer")
                    && event.input.as_deref() == Some("actual-second")
            }) {
                Ok(RawObserverAction::Continue)
            } else if events.iter().any(|event| {
                event.component.as_deref() == Some("card")
                    && event.message.as_deref() == Some("answer")
                    && event.input.as_deref() == Some("first")
            }) {
                Ok(RawObserverAction::CaptureInput(second.clone()))
            } else {
                Ok(RawObserverAction::CaptureInput(first.clone()))
            }
        },
    )
    .expect("capture chain relay");

    let blocks: Vec<_> = ledger_from_output(&output)
        .blocks
        .into_iter()
        .filter(|block| block.command == "echo capture-chain-ok")
        .collect();
    assert!(blocks.is_empty(), "{:?}", output.events);
    for answer in ["first", "actual-second"] {
        assert!(output.events.iter().any(|event| {
            event.message.as_deref() == Some("answer") && event.input.as_deref() == Some(answer)
        }));
    }
    assert!(!output.events.iter().any(|event| {
        event.message.as_deref() == Some("answer") && event.input.as_deref() == Some("second")
    }));
}

#[test]
fn raw_relay_capture_target_gone_discards_old_suffix_then_installs_new_target() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-capture-target-gone-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("capture-target-gone-test", &work_dir);
    config.native_mode = false;
    let first = RawInputCapture::Question {
        id: "question-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let second = RawInputCapture::Question {
        id: "question-2".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let third = RawInputCapture::Question {
        id: "question-3".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut calls_after_first_drain = 0;
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(b"first\necho abandoned-suffix-ok\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"answer-b\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::write(b"echo after-abandon-ok\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
        ],
        Vec::new(),
        move |events, _| {
            if events.iter().any(|event| {
                event.message.as_deref() == Some("answer")
                    && event.input.as_deref() == Some("answer-b")
            }) {
                Ok(RawObserverAction::Continue)
            } else if events
                .iter()
                .any(|event| event.message.as_deref() == Some("capture_drained"))
            {
                calls_after_first_drain += 1;
                if calls_after_first_drain == 1 {
                    Ok(RawObserverAction::Continue)
                } else {
                    Ok(RawObserverAction::CaptureInput(third.clone()))
                }
            } else if events.iter().any(|event| {
                event.message.as_deref() == Some("answer")
                    && event.input.as_deref() == Some("first")
            }) {
                Ok(RawObserverAction::CaptureInput(second.clone()))
            } else {
                Ok(RawObserverAction::CaptureInput(first.clone()))
            }
        },
    )
    .expect("capture target gone relay");

    let ledger = ledger_from_output(&output);
    assert_eq!(
        ledger
            .blocks
            .iter()
            .filter(|block| block.command == "echo abandoned-suffix-ok")
            .count(),
        0,
        "{:?}",
        output.events
    );
    assert_eq!(
        ledger
            .blocks
            .iter()
            .filter(|block| block.command == "echo after-abandon-ok")
            .count(),
        1,
        "{:?}",
        output.events
    );
    assert!(!output.events.iter().any(|event| {
        event.message.as_deref() == Some("answer")
            && event.input.as_deref() == Some("echo abandoned-suffix-ok")
    }));
    assert!(output.events.iter().any(|event| {
        event.message.as_deref() == Some("answer") && event.input.as_deref() == Some("answer-b")
    }));
}

#[test]
fn raw_relay_capture_eof_discards_old_generation_suffix() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-capture-eof-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("capture-eof-test", &work_dir);
    config.native_mode = false;
    let first = RawInputCapture::Question {
        id: "question-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let second = RawInputCapture::Question {
        id: "question-2".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(b"first\necho eof-suffix-ok\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
        ],
        Vec::new(),
        move |events, _| {
            if events
                .iter()
                .any(|event| event.message.as_deref() == Some("capture_drained"))
            {
                Ok(RawObserverAction::Continue)
            } else if events.iter().any(|event| {
                event.message.as_deref() == Some("answer")
                    && event.input.as_deref() == Some("first")
            }) {
                Ok(RawObserverAction::CaptureInput(second.clone()))
            } else {
                Ok(RawObserverAction::CaptureInput(first.clone()))
            }
        },
    )
    .expect("capture eof relay");

    assert_eq!(
        ledger_from_output(&output)
            .blocks
            .iter()
            .filter(|block| block.command == "echo eof-suffix-ok")
            .count(),
        0,
        "{:?}",
        output.events
    );
}

#[test]
fn raw_relay_capture_owned_input_overflow_is_visible_and_discarded() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-capture-overflow-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("capture-overflow-test", &work_dir);
    config.native_mode = false;
    let capture = RawInputCapture::Question {
        id: "question-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut input = b"yes\n#".to_vec();
    input.extend(std::iter::repeat_n(b'x', 64 * 1024));
    input.extend_from_slice(b"\necho capture-overflow-ok\n");
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::write(input),
            RawRelayAction::wait(Duration::from_millis(100)),
        ],
        Vec::new(),
        move |events, _| {
            if events
                .iter()
                .any(|event| event.message.as_deref() == Some("capture_overflow"))
            {
                Ok(RawObserverAction::Continue)
            } else {
                Ok(RawObserverAction::CaptureInput(capture.clone()))
            }
        },
    )
    .expect("capture overflow relay");

    let blocks: Vec<_> = ledger_from_output(&output)
        .blocks
        .into_iter()
        .filter(|block| block.command == "echo capture-overflow-ok")
        .collect();
    assert!(blocks.is_empty(), "{:?}", output.events);
    assert!(output
        .events
        .iter()
        .any(|event| event.message.as_deref() == Some("capture_overflow")));
}

#[test]
fn routing_c3_typed_passthrough_keeps_cjk_shell_owned() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-typed-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-typed", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(
        &config,
        std::io::Cursor::new("printf '%s\\n' 中文\n".as_bytes().to_vec()),
        &mut rendered,
    )
    .expect("typed passthrough");

    assert!(String::from_utf8_lossy(&rendered).contains("中文"));
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event
                .input
                .as_deref()
                .is_some_and(|input| input.contains("中文"))
    }));
}

#[test]
fn raw_relay_bash_ctrl_u_and_multiline_paste_execute_only_retained_input() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("SKIP: bash is unavailable");
        return;
    }

    for integration in [ShellIntegration::Native, ShellIntegration::Enhanced] {
        let root = tempfile::Builder::new()
            .prefix("cosh-shell-edit-paste-")
            .tempdir()
            .expect("editing root");
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join(".bashrc"), "PS1='editing$ '\n").expect("bashrc");
        let config = ShellHostConfig::new("edit-paste", root.path().join("work"))
            .with_integration(integration)
            .with_env("HOME", home.display().to_string())
            .with_env("LANG", "C.UTF-8")
            .with_env("LC_ALL", "C.UTF-8");
        let mut config = with_bracketed_paste_readline(config, true);
        config.raw_action_watchdog = Duration::from_secs(2);
        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_actions(
            &config,
            vec![
                RawRelayAction::wait(Duration::from_millis(100)),
                RawRelayAction::write(b"touch \"$HOME/discarded-command\"".to_vec()),
                RawRelayAction::write(vec![0x15]),
                RawRelayAction::line("printf 'retained\\n' > \"$HOME/edit-result\""),
                RawRelayAction::wait(Duration::from_millis(100)),
                RawRelayAction::write(
                    "\x1b[200~printf '第一行\\n' >> \"$HOME/edit-result\"\nprintf '第二行\\n' >> \"$HOME/edit-result\"\x1b[201~"
                        .as_bytes()
                        .to_vec(),
                ),
                RawRelayAction::line(""),
                RawRelayAction::line("exit"),
            ],
            &mut rendered,
        )
        .unwrap_or_else(|error| panic!("{integration:?}: {error}"));
        let terminal = String::from_utf8_lossy(&rendered);
        assert_eq!(output.exit_status, Some(0), "{integration:?}: {terminal}");
        assert!(
            !home.join("discarded-command").exists(),
            "{integration:?}: {terminal}"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("edit-result"))
                .unwrap_or_else(|error| panic!("{integration:?}: {error}: {terminal}")),
            "retained\n第一行\n第二行\n",
            "{integration:?}: {terminal}"
        );
        assert!(
            !output.events.iter().any(|event| {
                event.kind == ShellEventKind::UserInputIntercepted
                    && event.component.as_deref() == Some("natural_language")
            }),
            "{integration:?}: {terminal}"
        );
    }
}

#[test]
fn routing_c3_wrapped_paste_stays_shell_owned() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-wrapped-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-wrapped", &work_dir);
    let input = b"\x1b[200~printf WRAPPED_PASTE\x1b[201~\n".to_vec();
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(&config, std::io::Cursor::new(input), &mut rendered)
        .expect("wrapped paste");

    assert!(String::from_utf8_lossy(&rendered).contains("WRAPPED_PASTE"));
    assert!(!output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("natural_language")
    }));
}

#[test]
fn routing_c3_unwrapped_paste_uses_shell_newline_semantics() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-unwrapped-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-unwrapped", &work_dir);
    let mut rendered = Vec::new();
    run_raw_relay_bash(
        &config,
        std::io::Cursor::new(b"echo FIRST_LINE\necho SECOND_LINE\n".to_vec()),
        &mut rendered,
    )
    .expect("unwrapped multiline input");

    let rendered = String::from_utf8_lossy(&rendered);
    assert!(rendered.contains("FIRST_LINE"), "{rendered}");
    assert!(rendered.contains("SECOND_LINE"), "{rendered}");
}

#[test]
fn routing_c3_mirror_dirty_eof_never_appends_exit() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-mirror-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let side_effect = work_dir.join("must-not-exist");
    let config = ShellHostConfig::new("routing-c3-mirror", &work_dir);
    let input = format!("touch {}\x1b[D", shell_arg(&side_effect)).into_bytes();
    let output = run_raw_relay_bash(&config, std::io::Cursor::new(input), Vec::new())
        .expect("dirty mirror shutdown");

    assert!(!side_effect.exists());
    assert_ne!(output.exit_status, Some(0));
}

#[test]
fn routing_c3_paste_active_eof_never_executes_or_appends_exit() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-paste-eof-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-paste-eof", &work_dir);
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![RawRelayAction::write(b"\x1b[200~echo should-not-run\n")],
        Vec::new(),
    )
    .expect("paste-active EOF shutdown");

    assert_ne!(output.exit_status, Some(0));
    assert!(!output.events.iter().any(|event| {
        event
            .command
            .as_deref()
            .is_some_and(|command| command.contains("should-not-run") || command == "exit")
    }));
}

#[test]
fn routing_c3_mirror_oversize_eof_never_appends_exit() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-oversize-eof-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-oversize-eof", &work_dir);
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![RawRelayAction::write(vec![b'x'; 4097])],
        Vec::new(),
    )
    .expect("oversize mirror EOF shutdown");

    assert_ne!(output.exit_status, Some(0));
    assert!(!output
        .events
        .iter()
        .any(|event| { event.command.as_deref() == Some("exit") }));
}

#[test]
fn routing_c3_eof_partial_line_has_no_synthetic_pty_write() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-partial-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let side_effect = work_dir.join("partial-side-effect");
    let config = ShellHostConfig::new("routing-c3-partial", &work_dir);
    let input = format!("touch {}", shell_arg(&side_effect)).into_bytes();
    let output = run_raw_relay_bash(&config, std::io::Cursor::new(input), Vec::new())
        .expect("partial EOF shutdown");

    assert!(!side_effect.exists());
    assert_ne!(output.exit_status, Some(0));
}

#[test]
fn routing_c3_eof_session_shutdown_is_bounded_in_zsh() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-zsh-eof-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-zsh-eof", &work_dir);
    let started = Instant::now();
    let output = run_raw_relay_zsh_with_output_control(
        &config,
        std::io::Cursor::new(b"echo ZSH_PARTIAL".to_vec()),
        Vec::new(),
        |_, _| Ok(RawObserverAction::Continue),
    )
    .expect("zsh EOF shutdown");

    assert!(started.elapsed() < Duration::from_secs(5));
    assert_ne!(output.exit_status, Some(0));
}

#[test]
fn routing_c3_eof_submitted_no_drift_waits_for_command() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-submitted-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-submitted", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(
        &config,
        std::io::Cursor::new(b"sleep 0.2; echo FULL_LINE_DONE\n".to_vec()),
        &mut rendered,
    )
    .expect("submitted command then EOF");

    assert!(String::from_utf8_lossy(&rendered).contains("FULL_LINE_DONE"));
    assert_eq!(output.exit_status, Some(0));
}

struct RoutingC3ErrorReader;

impl Read for RoutingC3ErrorReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "routing-c3-reader-error",
        ))
    }
}

struct RoutingC3BytesThenErrorReader {
    bytes: Option<Vec<u8>>,
}

impl Read for RoutingC3BytesThenErrorReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(bytes) = self.bytes.take() {
            let length = bytes.len().min(buffer.len());
            buffer[..length].copy_from_slice(&bytes[..length]);
            return Ok(length);
        }
        std::thread::sleep(Duration::from_millis(200));
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "routing-c3-reader-error",
        ))
    }
}

#[test]
fn routing_c3_eof_error_preserves_reader_error() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-reader-error-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-reader-error", &work_dir);
    let error = run_raw_relay_bash(&config, RoutingC3ErrorReader, Vec::new())
        .expect_err("reader error must propagate");

    assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    assert_eq!(error.to_string(), "routing-c3-reader-error");
}

#[test]
fn routing_c3_driver_result_is_not_silently_discarded() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-driver-result-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-driver-result", &work_dir);
    let error = run_raw_relay_bash(
        &config,
        RoutingC3BytesThenErrorReader {
            bytes: Some(b"echo DRIVER_PREFIX_DONE\n".to_vec()),
        },
        Vec::new(),
    )
    .expect_err("driver result must reach host after relayed bytes");

    assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn routing_c3_signal_status_reaches_all_consumers() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-signal-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-signal", &work_dir);
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("sleep 30"),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::write(b"partial"),
        ],
        Vec::new(),
    )
    .expect("signal status relay");

    assert_eq!(output.exit_status, Some(129));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::ShellExited && event.exit_code == Some(129)
    }));
    assert!(output.events.iter().any(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
        ) && event.command.as_deref() == Some("sleep 30")
            && event.exit_code == Some(129)
    }));
}

#[test]
fn routing_c3_signal_status_kill_reaches_all_consumers() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-kill-signal-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-kill-signal", &work_dir);
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("trap '' HUP; sleep 30"),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::write(b"partial"),
        ],
        Vec::new(),
    )
    .expect("kill status relay");

    assert_eq!(output.exit_status, Some(137));
    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::ShellExited && event.exit_code == Some(137)
    }));
    assert!(output.events.iter().any(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed
        ) && event.command.as_deref() == Some("trap '' HUP; sleep 30")
            && event.exit_code == Some(137)
    }));
}

#[test]
fn routing_c3_eof_session_shutdown_kills_hup_ignoring_foreground_group() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-foreground-cleanup-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let pid_file = work_dir.join("foreground.pid");
    let command = format!(
        "bash -c 'trap \"\" HUP; echo $$ > {}; while :; do sleep 1; done'",
        pid_file.display()
    );
    let config = ShellHostConfig::new("routing-c3-foreground-cleanup", &work_dir);
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(command),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::write(b"partial"),
        ],
        Vec::new(),
    )
    .expect("foreground cleanup relay");

    assert_ne!(output.exit_status, Some(0));
    let pid = std::fs::read_to_string(&pid_file)
        .expect("foreground pid file")
        .trim()
        .parse::<i32>()
        .expect("foreground pid");
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        #[cfg(target_os = "linux")]
        if std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, suffix)| suffix.starts_with('Z'))
            })
            == Some(true)
        {
            return;
        }
        let result = unsafe { nix::libc::kill(pid, 0) };
        if result < 0 && io::Error::last_os_error().raw_os_error() == Some(nix::libc::ESRCH) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("foreground process {pid} survived EOF shutdown");
}

#[test]
fn routing_c3_explicit_draft_remains_the_only_multiline_agent_entry() {
    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-c3-draft-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("routing-c3-draft", &work_dir);
    let output = run_raw_relay_bash(&config, std::io::Cursor::new(b"??\n".to_vec()), Vec::new())
        .expect("explicit draft");

    assert!(output.events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("prompt_draft")
            && event.message.as_deref() == Some("open")
    }));
}
