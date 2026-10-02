use super::*;

#[test]
fn raw_relay_host_forwards_ctrl_c_and_keeps_shell_usable() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-relay-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-relay-test", &work_dir);
    let input = DelayedInput::new(vec![
        (b"sleep 5\n".to_vec(), Duration::ZERO),
        (vec![0x03], Duration::from_millis(250)),
        (
            b"echo after-ctrl-c\nls /path/that/does/not/exist\nexit\n".to_vec(),
            Duration::from_millis(100),
        ),
    ]);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(&config, input, &mut rendered).expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("after-ctrl-c"));
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);
    assert!(!rendered
        .windows(b"\x1b]1337;COSH;".len())
        .any(|window| window == b"\x1b]1337;COSH;"));

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    assert!(replayed_events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("control")
            && event.input.as_deref() == Some("ctrl_c")
    }));
    assert!(replayed_events
        .iter()
        .any(|event| event.kind == ShellEventKind::ShellExited));
}

#[test]
fn raw_relay_host_keeps_background_command_continuity() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-background-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-background-test", &work_dir);
    let input = std::io::Cursor::new(
        "sleep 0.2 &\n\
         echo after-background\n\
         exit\n",
    );
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(&config, input, &mut rendered).expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("after-background"));

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("sleep 0.2 &") && block.exit_code == 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-background") && block.exit_code == 0));
}

#[test]
fn raw_relay_host_applies_resize_actions() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-resize-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-resize-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::resize(40, 100),
            RawRelayAction::wait(Duration::from_millis(50)),
            RawRelayAction::line("stty size"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("40 100"), "{rendered_text}");

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("stty size"))
        .expect("stty size command block");
    assert_eq!(block.exit_code, 0);
    let output_ref = block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_ref_text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(output_ref_text.contains("40 100"), "{output_ref_text}");
}

#[test]
fn raw_relay_preserves_terminal_control_sequences_but_cleans_output_ref() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-display-control-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-display-control-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![RawRelayAction::line("printf 'before\\033[Kafter\\n'")],
        &mut rendered,
    )
    .expect("raw relay host");

    assert!(
        rendered
            .windows(b"\x1b[K".len())
            .any(|window| window == b"\x1b[K"),
        "{:?}",
        String::from_utf8_lossy(&rendered)
    );

    let ledger = ledger_from_output(&output);
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("printf 'before"))
        .expect("printf block");
    let output_ref = block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_ref_bytes = std::fs::read(output_ref).expect("output ref bytes");
    assert!(!output_ref_bytes
        .windows(b"\x1b[K".len())
        .any(|window| window == b"\x1b[K"));
    assert!(
        String::from_utf8_lossy(&output_ref_bytes).contains("beforeafter"),
        "{:?}",
        String::from_utf8_lossy(&output_ref_bytes)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn raw_relay_child_process_does_not_inherit_parent_pty_master() {
    if Command::new("bash").arg("--version").output().is_err()
        || Command::new("python3").arg("--version").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-fd-inherit-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("raw-fd-inherit-test", &work_dir);
    config.native_mode = false;
    let probe = r#"python3 - <<'PY'
import os
import stat

bad = []
for name in os.listdir("/dev/fd"):
    try:
        fd = int(name)
    except ValueError:
        continue
    if fd <= 2:
        continue
    try:
        st = os.fstat(fd)
    except OSError:
        continue
    if stat.S_ISCHR(st.st_mode) and os.major(st.st_rdev) == 15:
        bad.append(str(fd))
print("__PTY_MASTER_FDS__=" + ",".join(sorted(bad)))
PY
"#;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![RawRelayAction::write(probe.as_bytes().to_vec())],
        &mut rendered,
    )
    .expect("raw relay fd inheritance");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        rendered_text.contains("__PTY_MASTER_FDS__="),
        "{rendered_text}"
    );
    assert!(
        rendered_text.contains("__PTY_MASTER_FDS__=\r\n")
            || rendered_text.contains("__PTY_MASTER_FDS__=\n"),
        "child inherited PTY master fd:\n{rendered_text}"
    );

    let ledger = ledger_from_output(&output);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("python3 -") && block.exit_code == 0));
    let _ = std::fs::remove_dir_all(&work_dir);
}

#[test]
fn raw_relay_zsh_job_control_suspend_fg_and_interrupt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-job-control-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    // zsh selects vi keybindings from these conventional environment values.
    // The relay must not inject Bash Readline control bindings into that editor.
    let mut config = ShellHostConfig::new("zsh-job-control-test", &work_dir)
        .with_env("EDITOR", "vim")
        .with_env("VISUAL", "vim");
    config.native_mode = false;
    let mut rendered = Vec::new();
    let output = run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::line("sleep 5"),
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::write(vec![0x1a]),
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line("fg"),
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(400)),
            RawRelayAction::line("echo after-zsh-job-control"),
        ],
        &mut rendered,
    )
    .expect("zsh job control");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        rendered_text.contains("after-zsh-job-control"),
        "{rendered_text}"
    );
    assert_no_osc_marker(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command == "sleep 5" && block.exit_code != 0));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-zsh-job-control") && block.exit_code == 0));
}

#[test]
fn raw_relay_host_interrupts_python_repl_and_restores_terminal() {
    if Command::new("bash").arg("--version").output().is_err()
        || Command::new("python3").arg("--version").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-python-repl-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-python-repl-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("python3 -q"),
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("exit()"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("echo after-python-repl"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        rendered_text.contains("after-python-repl"),
        "{rendered_text}"
    );
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("python3 -q")));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-python-repl") && block.exit_code == 0));
}

#[test]
fn raw_relay_host_interrupts_node_repl_and_restores_terminal() {
    if Command::new("bash").arg("--version").output().is_err()
        || Command::new("node").arg("--version").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-node-repl-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-node-repl-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("node"),
            RawRelayAction::wait(Duration::from_millis(700)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line(".exit"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("echo after-node-repl"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("after-node-repl"), "{rendered_text}");
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(ledger.blocks.iter().any(|block| block.command == "node"));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-node-repl") && block.exit_code == 0));
}
