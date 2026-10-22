//! Python-driven removal prompts run only in the Python-enabled gate.

use super::support::{invoke, success, Fixture};
use serde_json::Value;
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf, process::Command};

fn captured(fixture: &Fixture, mode: &str, options: &[&str]) -> Value {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/removal_confirmation.py");
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg(script)
        .arg(mode)
        .arg(env!("CARGO_BIN_EXE_aw"))
        .args(["uninstall", "--prefix"])
        .arg(&fixture.prefix)
        .args(options)
        .env("XDG_STATE_HOME", fixture.root.join("package-state"))
        .current_dir(&fixture.root);
    let output = invoke(&command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn text(value: &Value, stream: &str) -> String {
    String::from_utf8(serde_json::from_value(value[stream].clone()).unwrap()).unwrap()
}

#[test]
fn tty_acceptance_runs_removal_and_quiet_does_not_bypass_the_prompt() {
    for options in [vec![], vec!["--quiet"]] {
        let f = Fixture::new();
        success(f.install().output().unwrap());
        let output = captured(&f, "accept", &options);
        assert_eq!(output["code"], 0);
        assert!(text(&output, "terminal").contains("[y/N]"));
        assert!(!f.prefix.join("bin/aw").exists());
        if options.contains(&"--quiet") {
            assert!(text(&output, "stdout").is_empty());
        }
    }
}

#[test]
fn tty_refusal_default_and_eof_preserve_every_installed_byte() {
    for mode in ["decline", "default", "eof"] {
        let f = Fixture::new();
        success(f.install().output().unwrap());
        let receipt = f.prefix.join(".aw-packages/aw-core.json");
        let before = fs::read(&receipt).unwrap();
        let inode = fs::metadata(&receipt).unwrap().ino();
        let output = captured(&f, mode, &[]);
        assert_eq!(output["code"], 1, "{mode}");
        let terminal = text(&output, "terminal");
        assert!(
            terminal.contains("[y/N]") && terminal.contains("no package operation was started")
        );
        assert!(!terminal.contains("rollback"));
        assert!(text(&output, "stdout").is_empty());
        assert_eq!(fs::read(&receipt).unwrap(), before);
        assert_eq!(fs::metadata(&receipt).unwrap().ino(), inode);
        assert!(f.prefix.join("bin/aw").is_file());
    }
}

#[test]
fn prompt_interrupts_retain_signal_exit_codes_without_starting_removal() {
    for (mode, code) in [("sigint", 130), ("sigterm", 143)] {
        let f = Fixture::new();
        success(f.install().output().unwrap());
        let output = captured(&f, mode, &[]);
        assert_eq!(output["code"], code);
        assert!(text(&output, "terminal").contains("confirmation interrupted"));
        assert!(text(&output, "stdout").is_empty());
        assert!(f.prefix.join(".aw-packages/aw-core.json").is_file());
    }
}

#[test]
fn mixed_terminal_channels_and_json_never_open_a_confirmation_prompt() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    for mode in ["stdin-tty", "stderr-tty", "json-tty"] {
        let options = if mode == "json-tty" {
            vec!["--json"]
        } else {
            vec![]
        };
        let output = captured(&f, mode, &options);
        assert_eq!(output["code"], 1);
        assert!(!text(&output, "terminal").contains("[y/N]"));
        assert!(f.prefix.join(".aw-packages/aw-core.json").exists());
    }
}

#[test]
fn prompt_metadata_escapes_terminal_and_bidi_control_characters() {
    let mut f = Fixture::new();
    f.prefix = f.root.join("prefix\n\u{1b}[2J\u{202e}");
    success(f.install().output().unwrap());
    let output = captured(&f, "default", &["--color", "never"]);
    let terminal = text(&output, "terminal");
    assert!(terminal.contains("prefix\\n\\u{1b}[2J\\u{202e}"));
    assert!(!terminal.contains('\u{1b}') && !terminal.contains('\u{202e}'));
}
