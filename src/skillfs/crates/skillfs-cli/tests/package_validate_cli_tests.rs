//! CLI behavior for the experimental package input and legacy source input.

use std::fs;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

fn validate() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_skillfs"));
    command.arg("validate");
    command
}

fn parse(out: &Output, code: i32) -> Value {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn accepts_package_with_nonfatal_diagnostics() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"example","version":"rev. 2"}"#,
    )
    .unwrap();
    let skill = root.path().join("skills/example");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: example\ndescription: Example\n---\n",
    )
    .unwrap();
    fs::write(
        root.path().join("mcp.json"),
        r#"{
            "$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
            "mcpServers":{"b":{"type":"stdio","command":"tool"},"a/~":{"type":"stdio"}}
        }"#,
    )
    .unwrap();

    let report = parse(
        &validate()
            .arg("--package")
            .arg(root.path())
            .args(["--format", "json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(report["experimental"], true);
    assert_eq!(report["accepted"], true);
    assert_eq!(
        report["manifest"],
        json!({
            "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
            "name": "example",
            "version": "rev. 2",
        })
    );
    assert_eq!(
        report["skills"],
        json!([{"name": "example", "path": "skills/example/SKILL.md"}])
    );
    assert_eq!(report["servers"], json!([{"name": "b", "type": "stdio"}]));
    assert_eq!(
        report["diagnostics"][0]["json_pointer"],
        "/mcpServers/a~1~0/command"
    );
    assert_eq!(report["diagnostics"][0]["severity"], "skip-server");
    let text = validate()
        .arg("--package")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(&format!(
        "Accepted package example ({}) [experimental]",
        root.path().display()
    )));
    assert!(text.contains("skip-server [mcp] mcp.json/mcpServers/a~1~0/command"));
}

#[test]
fn rejected_package_roots_and_manifest_still_write_json() {
    let root = TempDir::new().unwrap();
    let file = root.path().join("file");
    fs::write(&file, "not a directory").unwrap();
    for path in [root.path().to_path_buf(), file, root.path().join("missing")] {
        let out = validate()
            .arg("--package")
            .arg(path)
            .args(["-f", "json"])
            .output()
            .unwrap();
        let report = parse(&out, 1);
        assert_eq!(report["accepted"], false);
        assert_eq!(report["manifest"], Value::Null);
        assert_eq!(report["skills"], json!([]));
        assert_eq!(report["servers"], json!([]));
        assert_eq!(report["diagnostics"][0]["severity"], "reject-plugin");
    }
}

#[test]
fn package_input_is_exclusive_and_experimental() {
    assert_eq!(validate().output().unwrap().status.code(), Some(2));
    assert_eq!(
        validate()
            .args(["source", "--package", "pkg"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    let help = validate().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("--package <PACKAGE_ROOT>"));
    assert!(help.contains("[experimental]"));
}

#[test]
fn legacy_validate_retains_extensions_and_degraded_status() {
    let source = TempDir::new().unwrap();
    let skill = source.path().join("sample");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: sample\nversion: 1.0.0\ntags: [legacy]\n---\n",
    )
    .unwrap();
    let out = validate()
        .arg(source.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    let report = parse(&out, 0);
    assert_eq!(report["total"], 1);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["degraded"], 1);
    assert_eq!(report["warnings"][0]["name"], "sample");
    assert!(report.get("experimental").is_none());
}

#[test]
fn rejected_text_identifies_each_package_root_and_component() {
    let root = TempDir::new().unwrap();
    for (name, escaped) in [("alpha\nname", "alpha\\nname"), ("beta", "beta")] {
        let path = root.path().join(name);
        fs::create_dir(&path).unwrap();
        let out = validate().arg("--package").arg(&path).output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            format!(
                "Rejected package {}/{escaped} [experimental]",
                root.path().display()
            )
        );
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("reject-plugin [manifest] plugin.json (§5.1)"));
    }
}

#[test]
fn json_report_serializes_opaque_extension_numbers() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("plugin.json"),
        r#"{
            "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
            "name": "example",
            "extensions": {"org.example": 1e400}
        }"#,
    )
    .unwrap();
    let out = validate()
        .arg("--package")
        .arg(root.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    let object: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
        serde_json::from_str(&text).unwrap();
    assert_eq!(object["accepted"].get(), "true");
    assert!(object["manifest"].get().contains(r#""org.example": 1e400"#));
    assert_eq!(object["diagnostics"].get(), "[]");
}
