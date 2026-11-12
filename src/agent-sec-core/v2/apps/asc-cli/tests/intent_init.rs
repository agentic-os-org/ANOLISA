//! Local init acceptance tests: the command must work with no daemon, model or network.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn cli(directory: &Path) -> Command {
    let mut cli = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"));
    cli.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(directory)
        .args(["intent", "init"]);
    // `cargo llvm-cov` instruments the CLI child through this path; `env_clear` must not hide it.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        cli.env("LLVM_PROFILE_FILE", profile);
    }
    cli
}

fn stdout(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn init_generates_cedar_locally_without_a_daemon() {
    let directory = tempfile::tempdir().unwrap();
    let output = cli(directory.path())
        .args([
            "--allow-read",
            "/data/**",
            "--allow-write",
            "/data/out/**",
            "--deny-network",
            "--format",
            "cedar",
        ])
        .output()
        .unwrap();
    let text = stdout(&output);
    assert!(text.contains("not approved, not effective"));
    // The deterministic template needs no display escaping, even on a terminal.
    assert!(!text.contains("\\u{a}"));
    assert!(text.contains("permit (\n  principal is SecCore::Subject,"));
    assert!(text.contains("resource.path like \"/data/*\""));
    assert!(text.contains("resource.path like \"/data/out/*\""));
    assert!(text.contains("forbid"));
    assert!(text.contains("SecCore::Action::\"network.use\""));
    assert!(text.contains("SecCore::Network::\"all\""));
}

fn assert_direction_control_rejected(output: &Output, control: char) {
    assert_eq!(output.status.code(), Some(1), "control {control:?}");
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains(control));
    assert!(error.contains(&control.escape_unicode().to_string()));
}

#[test]
fn init_rejects_direction_controls_before_cedar_or_json_output() {
    let directory = tempfile::tempdir().unwrap();
    let spec_path = directory.path().join("spec.json");
    for control in [
        '\u{202e}', '\u{2066}', '\u{061c}', '\u{200e}', '\u{200f}', '\u{2028}', '\u{2029}',
        '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{2067}', '\u{2068}', '\u{2069}',
    ] {
        for (flag, field, suffix) in [
            ("--allow-read", "allow_read", ""),
            ("--allow-write", "allow_write", "/*"),
            ("--allow-exec", "allow_exec", "/**"),
            ("--deny-read", "deny_read", "/*"),
            ("--deny-write", "deny_write", "/**"),
            ("--deny-exec", "deny_exec", ""),
            ("--program", "programs", ""),
        ] {
            let path = format!("/data/{control}report.txt{suffix}");
            let mut spec = serde_json::json!({"allow_read":["/safe"]});
            spec[field] = serde_json::json!([path]);
            // Escaped JSON must be rejected after decoding, not only as literal bytes.
            let encoded = spec
                .to_string()
                .replace(control, &format!("\\u{:04x}", u32::from(control)));
            std::fs::write(&spec_path, encoded).unwrap();
            for format in ["cedar", "json"] {
                let flags = cli(directory.path())
                    .args(["--allow-read", "/safe", flag, &path, "--format", format])
                    .output()
                    .unwrap();
                assert_direction_control_rejected(&flags, control);
                let file = cli(directory.path())
                    .arg("--spec-file")
                    .arg(&spec_path)
                    .args(["--format", format])
                    .output()
                    .unwrap();
                assert_direction_control_rejected(&file, control);
            }
        }
    }
    // Ordinary Unicode paths still work and keep the template line breaks readable.
    let path = "/data/报告-مرحبا.txt";
    let text = stdout(
        &cli(directory.path())
            .args(["--allow-read", path])
            .output()
            .unwrap(),
    );
    assert!(text.contains(path));
    assert!(text.contains("permit (\n"));
    assert!(!text.contains("\\u{a}"));
    let output = cli(directory.path())
        .args(["--allow-read", path, "--format", "json"])
        .output()
        .unwrap();
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["spec"]["allow_read"][0], path);
    assert!(value["policy"].as_str().unwrap().contains(path));
}

#[test]
fn init_spec_file_json_conflicts_and_invalid_paths() {
    let directory = tempfile::tempdir().unwrap();
    let spec_path = directory.path().join("spec.json");
    std::fs::write(
        &spec_path,
        r#"{"allow_read":["/srv/app/**"],"programs":["/usr/bin/python3"]}"#,
    )
    .unwrap();
    let output = cli(directory.path())
        .arg("--spec-file")
        .arg(&spec_path)
        .args(["--format", "json"])
        .output()
        .unwrap();
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["format_version"], "asc.intent.init.v1");
    assert_eq!(value["spec"]["allow_read"][0], "/srv/app/**");
    assert!(
        value["policy"]
            .as_str()
            .unwrap()
            .contains("context.input.program == \"/usr/bin/python3\"")
    );
    assert!(
        value["schema_sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );

    // Rule flags and a spec file are mutually exclusive.
    let conflict = cli(directory.path())
        .arg("--spec-file")
        .arg(&spec_path)
        .args(["--allow-read", "/data"])
        .output()
        .unwrap();
    assert_ne!(conflict.status.code(), Some(0));

    // A relative path is rejected before any policy is emitted.
    let bad = cli(directory.path())
        .args(["--allow-read", "relative/path"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("canonical absolute path"));

    // No rule at all is an explicit error, not an empty deny-all policy.
    let empty = cli(directory.path()).output().unwrap();
    assert_eq!(empty.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&empty.stderr).contains("no rules"));
}
