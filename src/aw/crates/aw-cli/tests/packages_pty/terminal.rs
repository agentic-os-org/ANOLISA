//! Python-driven terminal checks run only in the Python-enabled gate.

use super::support::{invoke, success, Fixture};
use serde_json::Value;
use std::{path::PathBuf, process::Command};

fn captured(command: &Command, mode: &str, no_color: bool, term: &str) -> Value {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/terminal_capture.py");
    let mut wrapper = Command::new("/usr/bin/python3");
    wrapper
        .arg(fixture)
        .arg(mode)
        .arg(command.get_program())
        .args(command.get_args())
        .current_dir(command.get_current_dir().unwrap())
        .env("TERM", term)
        .env_remove("NO_COLOR");
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            wrapper.env(name, value);
        } else {
            wrapper.env_remove(name);
        }
    }
    if no_color {
        wrapper.env("NO_COLOR", "1");
    }
    let output = invoke(&wrapper);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn text(value: &Value, stream: &str) -> String {
    String::from_utf8(serde_json::from_value::<Vec<u8>>(value[stream].clone()).unwrap()).unwrap()
}

#[test]
fn actual_pty_obeys_auto_environment_and_explicit_color_choices() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    for (options, no_color, term, colored) in [
        (vec![], false, "xterm", true),
        (vec![], true, "xterm", false),
        (vec![], false, "dumb", false),
        (vec!["--color", "never"], false, "xterm", false),
        (vec!["--color", "always"], true, "dumb", true),
    ] {
        let output = captured(
            f.edit("remove-provider", &["--name", "absent"])
                .args(options),
            "pty",
            no_color,
            term,
        );
        assert_eq!(output["code"], 0);
        assert_eq!(text(&output, "stdout").contains('\u{1b}'), colored);
        assert!(text(&output, "stdout").contains("Configuration unchanged"));
        assert!(text(&output, "stderr").is_empty());
    }
    let output = captured(
        &f.edit("remove-provider", &["--name", "absent", "--json"]),
        "pty",
        false,
        "xterm",
    );
    assert_eq!(output["code"], 0);
    assert!(!text(&output, "stdout").contains('\u{1b}'));
    let report: Value = serde_json::from_str(&text(&output, "stdout")).unwrap();
    assert_eq!(report["result"]["changed"], false);
    let output = invoke(&f.edit(
        "remove-provider",
        &["--name", "absent", "--color", "always"],
    ));
    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"\x1b[32m"));
}

#[test]
fn output_write_failure_does_not_undo_a_completed_installation() {
    for json in [false, true] {
        let f = Fixture::new();
        let mut command = f.install();
        if json {
            command.arg("--json");
        }
        let output = captured(&command, "closed", false, "xterm");
        assert_eq!(output["code"], 1);
        assert!(text(&output, "stdout").is_empty());
        let diagnostic = text(&output, "stderr");
        assert!(!diagnostic.contains("rollback") && !diagnostic.contains("panicked"));
        assert!(diagnostic.starts_with("aw:"));
        assert!(f.prefix.join(".aw-packages/aw-core.json").is_file());
        assert!(f.prefix.join("bin/aw").is_file());
    }
    let f = Fixture::new();
    let output = captured(
        f.install().arg("--verbose"),
        "closed-stderr",
        false,
        "xterm",
    );
    assert_eq!(output["code"], 1);
    assert!(text(&output, "stdout").starts_with("Installed "));
    assert!(text(&output, "stderr").is_empty());
    assert!(f.prefix.join(".aw-packages/aw-core.json").is_file());
    assert!(f.prefix.join("bin/aw").is_file());
}
