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
