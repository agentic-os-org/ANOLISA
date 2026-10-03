use super::*;

#[test]
#[ignore = "fullscreen TUI programs can block the default package gate; run manually for PTY smoke"]
fn raw_relay_host_runs_fullscreen_programs_and_keeps_shell_usable() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-fullscreen-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&work_dir).expect("work dir");
    let vim_file = work_dir.join("vim.txt");
    std::fs::write(&vim_file, "").expect("vim file");
    let config = ShellHostConfig::new("raw-fullscreen-test", &work_dir);

    let has_less = Command::new("less").arg("--version").output().is_ok();
    let has_vim = Command::new("vim").arg("--version").output().is_ok();

    let mut actions = Vec::new();

    if has_less {
        actions.push(RawRelayAction::line("seq 1 200 | TERM=xterm-256color less"));
        actions.push(RawRelayAction::wait(Duration::from_millis(500)));
        actions.push(RawRelayAction::write(b"q".to_vec()));
        actions.push(RawRelayAction::wait(Duration::from_millis(500)));
        actions.push(RawRelayAction::line("echo after-less"));
        actions.push(RawRelayAction::wait(Duration::from_millis(500)));
    }

    if has_vim {
        actions.push(RawRelayAction::line(format!(
            "vim -Nu NONE -n {}",
            shell_arg(&vim_file)
        )));
        actions.push(RawRelayAction::wait(Duration::from_millis(500)));
        actions.push(RawRelayAction::write(b"\x1b:q!\n".to_vec()));
        actions.push(RawRelayAction::wait(Duration::from_millis(500)));
        actions.push(RawRelayAction::line("echo after-vim"));
        actions.push(RawRelayAction::wait(Duration::from_millis(500)));
    }

    if actions.is_empty() {
        return;
    }

    let mut rendered = Vec::new();
    let output =
        run_raw_relay_bash_with_actions(&config, actions, &mut rendered).expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert_no_osc_marker(&rendered);

    if has_less {
        assert!(rendered_text.contains("after-less"), "{rendered_text}");
    }
    if has_vim {
        assert!(rendered_text.contains("after-vim"), "{rendered_text}");
    }

    let ledger = ledger_from_output(&output);
    if has_less {
        assert!(ledger.blocks.iter().any(|block| block
            .command
            .contains("seq 1 200 | TERM=xterm-256color less")
            && block.exit_code == 0));
    }
    if has_vim {
        assert!(ledger
            .blocks
            .iter()
            .any(|block| block.command.contains("vim -Nu NONE -n") && block.exit_code == 0));
    }
}

#[test]
fn raw_relay_host_runs_less_and_restores_terminal() {
    if Command::new("bash").arg("--version").output().is_err()
        || Command::new("less").arg("--version").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-less-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-less-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("seq 1 200 | TERM=xterm-256color less"),
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::write(b"q".to_vec()),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line("echo after-less"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("after-less"), "{rendered_text}");
    assert_fullscreen_terminal_modes_balanced(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(ledger.blocks.iter().any(|block| block
        .command
        .contains("seq 1 200 | TERM=xterm-256color less")));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-less") && block.exit_code == 0));
}

#[test]
fn raw_relay_host_runs_top_and_keeps_shell_usable() {
    if Command::new("bash").arg("--version").output().is_err()
        || Command::new("top").arg("-h").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-top-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-top-test", &work_dir);
    let mut rendered = Vec::new();
    let _output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(
                "top -l 1 2>/dev/null || top -bn1 2>/dev/null || echo top-skipped",
            ),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(b"q".to_vec()),
            RawRelayAction::wait(Duration::from_millis(100)),
            RawRelayAction::line("echo after-top"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("after-top"), "{rendered_text}");
    assert_no_osc_marker(&rendered);
    assert_fullscreen_terminal_modes_balanced(&rendered);
}

#[test]
fn raw_relay_host_runs_batchmode_ssh_without_swallowing_shell() {
    if Command::new("bash").arg("--version").output().is_err()
        || Command::new("ssh").arg("-V").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-ssh-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-ssh-test", &work_dir);
    let ssh_command = "ssh -o BatchMode=yes -o ConnectTimeout=1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null 127.0.0.1 true";
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(ssh_command),
            RawRelayAction::wait(Duration::from_millis(1500)),
            RawRelayAction::line("echo after-ssh"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("after-ssh"), "{rendered_text}");
    assert_no_osc_marker(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("ssh -o BatchMode=yes")));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-ssh") && block.exit_code == 0));
}

#[test]
fn raw_relay_host_shows_isolated_sudo_prompt_and_keeps_shell_usable() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::path::PathBuf::from("/tmp").join(format!(
        "cosh-shell-raw-sudo-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let fake_bin_dir = work_dir.join("fake-bin");
    std::fs::create_dir_all(&fake_bin_dir).expect("fake bin dir");
    let fake_sudo = fake_bin_dir.join("sudo");
    std::fs::write(
        &fake_sudo,
        "#!/bin/sh\n\
         prompt='[sudo] password for cosh: '\n\
         while [ \"$#\" -gt 0 ]; do\n\
           case \"$1\" in\n\
             -p) shift; prompt=\"$1\" ;;\n\
           esac\n\
           shift || true\n\
         done\n\
         printf '%s' \"$prompt\" >&2\n\
         IFS= read -r _password\n\
         exit 1\n",
    )
    .expect("fake sudo script");
    make_executable(&fake_sudo);

    let config = ShellHostConfig::new("raw-sudo-test", &work_dir);
    let command = format!(
        "PATH={}:$PATH sudo -p '[sudo] password for cosh: ' true",
        shell_arg(&fake_bin_dir)
    );
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(command),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("echo after-sudo"),
            RawRelayAction::wait(Duration::from_millis(300)),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("sudo"), "{rendered_text}");
    assert!(
        rendered_text.contains("password for cosh:"),
        "{rendered_text}"
    );
    assert!(rendered_text.contains("after-sudo"), "{rendered_text}");
    assert_no_osc_marker(&rendered);
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    let sudo_block = ledger
        .blocks
        .iter()
        .find(|block| block.command.contains("sudo -p"))
        .expect("sudo command block");
    assert_ne!(sudo_block.exit_code, 0);
    let output_ref = sudo_block
        .output
        .terminal_output_ref
        .as_deref()
        .expect("terminal output ref");
    let output_ref_text = std::fs::read_to_string(output_ref).expect("output ref text");
    assert!(!output_ref_text.contains("\x1b]1337;COSH;"));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-sudo") && block.exit_code == 0));
}

#[test]
fn raw_relay_zsh_tty_password_prompt_ctrl_c_keeps_shell_usable() {
    if Command::new("zsh").arg("--version").output().is_err()
        || Command::new("python3").arg("--version").output().is_err()
    {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-zsh-tty-password-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let fake_bin_dir = work_dir.join("fake-bin");
    std::fs::create_dir_all(&fake_bin_dir).expect("fake bin dir");
    let fake_sudo = fake_bin_dir.join("sudo");
    std::fs::write(
        &fake_sudo,
        r#"#!/usr/bin/env python3
import os
import signal
import sys
import termios

signal.alarm(5)
tty = os.open("/dev/tty", os.O_RDWR)
prompt = "[sudo] password for cosh: "
args = sys.argv[1:]
for idx, arg in enumerate(args):
    if arg == "-p" and idx + 1 < len(args):
        prompt = args[idx + 1]
os.write(tty, prompt.encode())
old = termios.tcgetattr(tty)
new = old[:]
new[3] &= ~termios.ECHO
try:
    termios.tcsetattr(tty, termios.TCSANOW, new)
    os.read(tty, 1024)
finally:
    termios.tcsetattr(tty, termios.TCSANOW, old)
    os.write(tty, b"\n")
sys.exit(1)
"#,
    )
    .expect("fake sudo script");
    make_executable(&fake_sudo);

    let mut config = ShellHostConfig::new("zsh-tty-password-test", &work_dir);
    config.native_mode = false;
    let command = format!(
        "PATH={}:$PATH sudo -p '[sudo] password for cosh: ' true",
        shell_arg(&fake_bin_dir)
    );
    let mut rendered = Vec::new();
    let output = run_raw_relay_zsh_with_actions(
        &config,
        vec![
            RawRelayAction::line(command),
            RawRelayAction::wait(Duration::from_millis(600)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(400)),
            RawRelayAction::line("echo after-zsh-tty-password"),
        ],
        &mut rendered,
    )
    .expect("zsh tty password prompt");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        rendered_text.contains("password for cosh:"),
        "{rendered_text}"
    );
    assert!(
        rendered_text.contains("after-zsh-tty-password"),
        "{rendered_text}"
    );
    assert_no_osc_marker(&rendered);
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("sudo -p") && block.exit_code != 0));
    assert!(ledger.blocks.iter().any(|block| {
        block.command.contains("echo after-zsh-tty-password") && block.exit_code == 0
    }));
}
