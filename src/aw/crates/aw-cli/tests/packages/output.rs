//! Management reports expose outcomes, not private configuration or runtime claims.

pub(super) use super::support::invoke;
use super::*;

fn report(command: &Command, code: i32, action: &str) -> Value {
    let output = invoke(command);
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["format"], 1);
    assert_eq!(value["command"], action);
    assert_eq!(
        value["status"],
        if code == 0 { "succeeded" } else { "failed" }
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-api-key"));
    assert!(value.get("changes").is_none());
    assert!(value.get("runtime").is_none());
    value
}

#[test]
fn install_init_and_uninstall_report_local_outcomes_without_starting_a_runtime() {
    let f = Fixture::new();
    let installed = report(f.install().arg("--json"), 0, "install");
    assert_eq!(
        installed["result"],
        json!({"kind":"installed", "prefix":f.prefix})
    );
    assert!(f.prefix.join(".aw-packages/aw-core.json").exists());
    let configured = report(f.init().arg("--json"), 0, "config init");
    assert_eq!(
        configured["result"],
        json!({"kind":"configured", "config":f.config, "provider_template":"none"})
    );
    assert!(!f.state.exists());
    let bytes = fs::read(&f.config).unwrap();
    let removed = report(
        f.command()
            .args(["uninstall", "--yes", "--json", "--prefix"])
            .arg(&f.prefix),
        0,
        "uninstall",
    );
    assert_eq!(removed["result"], json!({"kind":"uninstalled"}));
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    assert!(!f.prefix.join("bin/aw").exists());
}

#[test]
fn offline_edits_report_changed_and_noop_without_echoing_definitions() {
    let f = Fixture::new();
    report(f.install().arg("--json"), 0, "install");
    report(f.init().arg("--json"), 0, "config init");
    fs::write(f.root.join("provider.json"), serde_json::to_vec(&json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio", "location":"agent", "argv":["/unexecuted/private-api-key"]},
        "timeout_ms":1000, "max_output_bytes":4096, "config":{"token":"private-api-key"}
    })).unwrap()).unwrap();
    let provider = [
        "--name",
        "external",
        "--definition",
        "provider.json",
        "--json",
    ];
    for changed in [true, false] {
        let before = fs::read(&f.config).unwrap();
        let inode = fs::metadata(&f.config).unwrap().ino();
        let value = report(&f.edit("add-provider", &provider), 0, "config add-provider");
        assert_eq!(
            value["result"],
            json!({"kind":"configuration_edited", "changed":changed})
        );
        if !changed {
            assert_eq!(fs::read(&f.config).unwrap(), before);
            assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
        }
    }
    fs::write(f.root.join("event.json"), r#"{"enabled":false,"steps":[]}"#).unwrap();
    fs::write(f.root.join("hook.json"), r#"{"id":"first","provider":"external","operation":"observe","effects":["observe"],"on_error":"report"}"#).unwrap();
    for (action, options) in [
        (
            "add-event",
            vec!["--event", "tool.before", "--definition", "event.json"],
        ),
        (
            "add-hook",
            vec!["--event", "tool.before", "--definition", "hook.json"],
        ),
        (
            "remove-hook",
            vec!["--event", "tool.before", "--id", "first"],
        ),
        ("remove-provider", vec!["--name", "external"]),
    ] {
        let value = report(
            f.edit(action, &options).arg("--json"),
            0,
            &format!("config {action}"),
        );
        assert_eq!(value["result"]["changed"], true);
    }
    assert!(!f.state.exists());
}

#[test]
fn operation_failures_use_nonzero_json_without_private_error_details() {
    let f = Fixture::new();
    report(f.install().arg("--json"), 0, "install");
    report(f.init().arg("--json"), 0, "config init");
    let before = fs::read(&f.config).unwrap();
    fs::write(
        f.root.join("invalid.json"),
        r#"{"protocol":"private-api-key"}"#,
    )
    .unwrap();
    let error = report(
        &f.edit(
            "add-provider",
            &[
                "--name",
                "private-api-key",
                "--definition",
                "invalid.json",
                "--json",
            ],
        ),
        1,
        "config add-provider",
    );
    assert_eq!(error["error"]["code"], "package_contract");
    assert!(error.get("result").is_none());
    assert_eq!(fs::read(&f.config).unwrap(), before);
    let error = report(
        &f.edit(
            "add-provider",
            &[
                "--name",
                "external",
                "--definition",
                "private-api-key-missing.json",
                "--json",
            ],
        ),
        1,
        "config add-provider",
    );
    assert_eq!(error["error"]["code"], "io");
    assert_eq!(fs::read(&f.config).unwrap(), before);
    fs::write(
        f.bundle.join("manifest.json"),
        "private-api-key-invalid-json",
    )
    .unwrap();
    let error = report(f.install().arg("--json"), 1, "install");
    assert_eq!(error["error"]["code"], "invalid_json");
    assert!(f.prefix.join(".aw-packages/aw-core.json").exists());
}

#[test]
fn json_remains_local_and_syntax_errors_do_not_emit_result_objects() {
    let f = Fixture::new();
    for args in [
        vec!["install", "--json"],
        vec![
            "install", "--prefix", "/missing", "--bundle", "/missing", "--json", "--json",
        ],
        vec![
            "install",
            "--config",
            "missing.yaml",
            "--agent",
            "qoder",
            "--json",
        ],
        vec![
            "agent",
            "connect",
            "--config",
            "missing.yaml",
            "--agent",
            "qoder",
            "--json",
        ],
        vec!["config", "show", "--config", "/missing", "--json"],
        vec!["validate", "--config", "/missing", "--json"],
        vec!["policy", "--json"],
    ] {
        let output = invoke(f.command().args(args));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    assert!(!f.prefix.exists());
    assert!(!f.config.exists());
}

#[test]
fn json_encodes_control_characters_in_selected_paths() {
    let mut f = Fixture::new();
    f.prefix = f.root.join("prefix\n\u{1b}[2J");
    let output = invoke(f.install().arg("--json"));
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    assert!(!output.stdout.contains(&0x1b));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["prefix"], f.prefix.to_str().unwrap());
}
