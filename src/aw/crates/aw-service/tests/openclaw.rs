//! OpenClaw registration and daemon transport without requiring a native installation.
#![cfg(target_os = "linux")]

// Reuse the bounded daemon/process owner; its Qoder-specific helpers are unused.
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use support::{Fixture, Service};

fn source(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/openclaw")
        .join(name)
}

fn document(fixture: &Fixture) -> Value {
    let mut probe = Command::new("node");
    probe.args(["-p", "process.execPath"]);
    let output = fixture.run(probe);
    assert!(output.status.success(), "Node executable probe failed");
    let node = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(node.is_absolute());
    let native = fixture.root.join("native with spaces");
    fs::create_dir(&native).unwrap();
    let executable = native.join("openclaw.mjs");
    fs::copy(source("gateway.mjs"), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(fixture.root.join("profile")).unwrap();
    fs::write(
        fixture.root.join("profile/credential-marker"),
        "persistent-auth",
    )
    .unwrap();
    let settings = json!({"gateway":{"mode":"local"},"plugins":{"allow":["existing"],
        "load":{"paths":["/existing/plugin"]},"entries":{"existing":{"enabled":true,"config":{"marker":"kept"}}}}});
    fs::write(fixture.root.join("native.json"), settings.to_string()).unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"openclaw":{"adapter":"openclaw","argv":[node,executable,"gateway","run"]}});
    document
}

fn step(
    fixture: &Fixture,
    document: &mut Value,
    event: &str,
    label: &str,
    behavior: &str,
    structured: bool,
) {
    document["spec"]["providers"][label] = json!({"protocol":if structured {"aw-provider/v1alpha1"} else {"native-hook/v1alpha1"},
        "transport":{"type":"stdio","location":"agent","argv":["/usr/bin/python3",source("action.py"),"--root",fixture.root,"--label",label,"--behavior",behavior]},
        "timeout_ms":4500,"max_output_bytes":65536,"config":{}});
    if document["spec"]["events"][event].is_null() {
        document["spec"]["events"][event] = json!({"enabled":true,"steps":[]});
    }
    let mut value = json!({"id":label,"provider":label,"on_error":if event=="tool.before" {"block"}else{"report"}});
    if structured {
        value["operation"] = json!(if event == "tool.before" {
            "check"
        } else {
            "record"
        });
        value["effects"] = if event == "tool.before" {
            json!(["observe", "block"])
        } else {
            json!(["observe"])
        };
    } else {
        value["native"] = json!({});
    }
    document["spec"]["events"][event]["steps"]
        .as_array_mut()
        .unwrap()
        .push(value);
}

fn command(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    // Exercise installations where Node exists only on the test runner's PATH.
    command.env("PATH", fixture.root.join("home"));
    command
        .args(["run", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "openclaw", "--native-settings"])
        .arg(fixture.root.join("native.json"))
        .arg("--native-state-dir")
        .arg(fixture.root.join("profile"));
    command
}

#[test]
fn plugin_callbacks_preserve_configuration_profile_raw_events_and_native_parallelism() {
    let fixture = Fixture::new();
    let mut config = document(&fixture);
    step(
        &fixture,
        &mut config,
        "tool.before",
        "first",
        "observe",
        false,
    );
    step(
        &fixture,
        &mut config,
        "tool.before",
        "second",
        "observe",
        false,
    );
    step(
        &fixture,
        &mut config,
        "tool.after",
        "after-first",
        "barrier",
        false,
    );
    step(
        &fixture,
        &mut config,
        "tool.after",
        "after-second",
        "barrier",
        false,
    );
    let original = fs::read(fixture.root.join("native.json")).unwrap();
    let service = Service::start(&fixture, &config);
    let report = fixture.successful(command(&fixture));
    assert_eq!(report["blocked"], false);
    assert_eq!(report["credential"], true);
    assert_eq!(report["state"], json!(fixture.root.join("profile")));
    assert_eq!(report["home"], json!(fixture.root.join("home")));
    assert_eq!(report["readOnly"], "1");
    assert!(fixture.root.join("tool-ran").exists());
    let generated: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("generated.json")).unwrap()).unwrap();
    assert_eq!(
        generated["plugins"]["entries"]["existing"]["config"]["marker"],
        "kept"
    );
    assert_eq!(generated["plugins"]["load"]["paths"][0], "/existing/plugin");
    assert_eq!(
        generated["plugins"]["allow"],
        json!(["existing", "aw-native-hooks"])
    );
    assert_eq!(
        fs::read(fixture.root.join("native.json")).unwrap(),
        original
    );
    let calls: Vec<Value> = fs::read_to_string(fixture.root.join("raw-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0]["digest"], calls[1]["digest"]);
    assert_eq!(calls[2]["digest"], calls[3]["digest"]);
    assert!(fixture.root.join("barrier-after-first").exists());
    assert!(fixture.root.join("barrier-after-second").exists());
    service.released(&fixture);
}

#[test]
fn structured_and_native_before_block_prevent_the_tool_and_after_callbacks() {
    for structured in [false, true] {
        let fixture = Fixture::new();
        let mut config = document(&fixture);
        step(
            &fixture,
            &mut config,
            "tool.before",
            "policy",
            "block",
            structured,
        );
        step(
            &fixture,
            &mut config,
            "tool.after",
            "after",
            "observe",
            false,
        );
        let service = Service::start(&fixture, &config);
        if structured {
            let allowed = fixture.successful(command(&fixture));
            assert_eq!(allowed["blocked"], false);
            fs::remove_file(fixture.root.join("tool-ran")).unwrap();
            fs::remove_file(fixture.root.join("raw-calls.jsonl")).unwrap();
            service.released(&fixture);
        }
        let mut launch = command(&fixture);
        launch.env("FAKE_SCENARIO", "DENY");
        let report = fixture.successful(launch);
        assert_eq!(report["blocked"], true);
        assert!(!fixture.root.join("tool-ran").exists());
        if let Ok(records) = fs::read_to_string(fixture.root.join("raw-calls.jsonl")) {
            assert!(!records.contains("after_tool_call"));
        }
        service.released(&fixture);
    }
}

#[test]
fn serial_steps_share_one_daemon_deadline_and_failure_blocks() {
    let fixture = Fixture::new();
    let mut config = document(&fixture);
    config["spec"]["execution"]["default_event_budget_ms"] = json!(600);
    step(
        &fixture,
        &mut config,
        "tool.before",
        "first",
        "sleep",
        false,
    );
    step(
        &fixture,
        &mut config,
        "tool.before",
        "second",
        "sleep",
        false,
    );
    let service = Service::start(&fixture, &config);
    let report = fixture.successful(command(&fixture));
    assert_eq!(report["blocked"], true);
    assert!(!fixture.root.join("tool-ran").exists());
    service.released(&fixture);
}

#[test]
fn native_commands_receive_host_environment_while_providers_keep_launch_context() {
    let fixture = Fixture::new();
    let mut config = document(&fixture);
    step(
        &fixture,
        &mut config,
        "tool.before",
        "policy",
        "observe",
        true,
    );
    step(
        &fixture,
        &mut config,
        "tool.before",
        "native",
        "observe",
        false,
    );
    let service = Service::start(&fixture, &config);
    let mut launch = command(&fixture);
    launch.env("AW_OPENCLAW_NATIVE_ENV_FIXTURE", "launch-context");
    let report = fixture.successful(launch);
    assert_eq!(report["blocked"], false);
    let raw: Value = serde_json::from_str(
        fs::read_to_string(fixture.root.join("raw-calls.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(raw["native_environment"], "loaded-by-native-host");
    let pinned: Value = serde_json::from_slice(
        &fs::read(fixture.root.join("provider-environment-invoke.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(pinned, "launch-context");
    service.released(&fixture);
}

#[test]
fn plugin_node_contract_tests_pass() {
    let fixture = Fixture::new();
    let mut command = Command::new("node");
    command
        .arg("--test")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/openclaw.mjs"));
    let output = fixture.run(command);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
