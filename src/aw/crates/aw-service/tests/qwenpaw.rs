//! Synthetic App lifecycle covers service wiring; native middleware tests use AgentScope.
#![cfg(target_os = "linux")]

// This adapter shares lifecycle helpers with the broader launcher test suite.
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
use support::{Fixture, Service, LITERAL};

fn action() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qwenpaw/action.py")
}

fn setup(fixture: &Fixture) -> Value {
    let host = fixture.root.join("fake qwenpaw");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qwenpaw/host.py"),
        &host,
    )
    .unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        fixture.root.join("home/config.json"),
        "{\"theme\":\"preserved\"}\n",
    )
    .unwrap();
    fs::create_dir_all(fixture.root.join("home/plugins/existing")).unwrap();
    fs::write(fixture.root.join("home/plugins/existing/data"), "unchanged").unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"qwenpaw":{"adapter":"qwenpaw","argv":[host,"app"]}});
    document
}

fn launch(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .args(["run", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qwenpaw"])
        .env("QWENPAW_WORKING_DIR", fixture.root.join("home"));
    command
}

fn native(fixture: &Fixture, document: &mut Value, event: &str, label: &str) {
    document["spec"]["providers"][label] = json!({
        "protocol":"native-hook/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":["/usr/bin/python3",action(),fixture.root,"native",label,LITERAL]},
        "timeout_ms":4500,"max_output_bytes":4096,"config":{},
    });
    if document["spec"]["events"][event].is_null() {
        document["spec"]["events"][event] = json!({"enabled":true,"steps":[]});
    }
    document["spec"]["events"][event]["steps"].as_array_mut().unwrap().push(
        json!({"id":label,"provider":label,"native":{},"on_error":if event == "tool.before" {"block"} else {"report"}}),
    );
}

fn preserved(fixture: &Fixture) {
    assert_eq!(
        fs::read_to_string(fixture.root.join("home/config.json")).unwrap(),
        "{\"theme\":\"preserved\"}\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("home/plugins/existing/data")).unwrap(),
        "unchanged"
    );
    assert!(!fixture.root.join("home/plugins/aw-native").exists());
}

#[test]
fn native_commands_keep_onion_order_literal_argv_and_profile() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    for (event, label) in [
        ("tool.before", "before-a"),
        ("tool.before", "before-b"),
        ("tool.after", "after-a"),
        ("tool.after", "after-b"),
    ] {
        native(&fixture, &mut document, event, label);
    }
    let service = Service::start(&fixture, &document);
    let result = fixture.successful(launch(&fixture));
    assert_eq!(result["tool_executed"], true);
    let order: Vec<Value> = fs::read_to_string(fixture.root.join("order.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        order,
        vec![
            json!("before-a"),
            json!("before-b"),
            json!("after-b"),
            json!("after-a")
        ]
    );
    for label in ["before-a", "before-b", "after-a", "after-b"] {
        let raw: Value = serde_json::from_slice(
            &fs::read(fixture.root.join(format!("{label}-raw.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(raw["session_id"], "native-session");
        assert_eq!(raw["tool_call"]["id"], "call-1");
        assert!(raw["tool_call"]["input"].is_string());
        let argv: Value = serde_json::from_slice(
            &fs::read(fixture.root.join(format!("{label}-argv.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(argv, json!([LITERAL]));
    }
    assert!(!fixture.root.join("NEVER").exists());
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn provider_block_is_adopted_and_observed_after_without_a_tool() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    fixture.provider(&mut document);
    document["spec"]["providers"]["policy"]["transport"]["argv"] = json!([
        "/usr/bin/python3",
        action(),
        fixture.root,
        "provider",
        "policy"
    ]);
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("FAKE_QWENPAW_DENY", "1");
    let result = fixture.successful(command);
    assert_eq!(result["tool_executed"], false);
    assert_eq!(result["results"][0]["state"], "denied");
    assert!(!fixture.root.join("tool-executed").exists());
    let event: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("tool.after-provider.json")).unwrap())
            .unwrap();
    assert_eq!(event["tool"]["result"]["state"], "denied");
    assert_eq!(event["tool"]["input"]["nested"][1], Value::Null);
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn native_block_and_ask_have_explicit_outcomes() {
    for label in ["block", "ask"] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        native(&fixture, &mut document, "tool.before", label);
        let service = Service::start(&fixture, &document);
        let output = fixture.run(launch(&fixture));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!fixture.root.join("tool-executed").exists());
        if label == "ask" {
            assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported_approval"));
        }
        preserved(&fixture);
        service.released(&fixture);
    }
}

#[test]
fn report_callback_failures_preserve_before_execution_and_after_result() {
    for label in ["ask", "killed"] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        for event in ["tool.before", "tool.after"] {
            native(&fixture, &mut document, event, label);
            document["spec"]["events"][event]["steps"][0]["on_error"] = json!("report");
        }
        let service = Service::start(&fixture, &document);
        let output = fixture.run(launch(&fixture));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tool_executed"], true);
        assert_eq!(report["results"][0]["state"], "success");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostics.contains("AW tool.before step"));
        assert!(diagnostics.contains("AW tool.after step"));
        assert!(diagnostics.contains("on_error=report"));
        preserved(&fixture);
        service.released(&fixture);
    }
}

#[test]
fn unsupported_entries_and_existing_plugin_are_rejected_before_service_creation() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    native(&fixture, &mut document, "tool.before", "before");
    for args in [
        json!(["acp"]),
        json!(["tui"]),
        json!([]),
        json!(["app", "--reload"]),
        json!(["app", "--workers", "2"]),
    ] {
        document["spec"]["agents"]["qwenpaw"]["argv"] = json!([fixture.root.join("fake qwenpaw")]);
        document["spec"]["agents"]["qwenpaw"]["argv"]
            .as_array_mut()
            .unwrap()
            .extend(args.as_array().unwrap().iter().cloned());
        fixture.save(&document);
        assert!(!fixture.run(launch(&fixture)).status.success());
        assert!(!fixture.root.join("state").exists());
    }
    document["spec"]["agents"]["qwenpaw"]["argv"] =
        json!([fixture.root.join("fake qwenpaw"), "app"]);
    fixture.save(&document);
    fs::create_dir(fixture.root.join("home/plugins/aw-native")).unwrap();
    fs::write(
        fixture.root.join("home/plugins/aw-native/owned-by-user"),
        "preserve",
    )
    .unwrap();
    assert!(!fixture.run(launch(&fixture)).status.success());
    assert_eq!(
        fs::read_to_string(fixture.root.join("home/plugins/aw-native/owned-by-user")).unwrap(),
        "preserve"
    );
    assert!(!fixture.root.join("state").exists());
}

#[test]
fn app_exit_before_readiness_releases_instance_and_owned_files() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    native(&fixture, &mut document, "tool.before", "before");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("FAKE_QWENPAW_EARLY_EXIT", "1");
    assert!(!fixture.run(command).status.success());
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn serial_steps_share_one_event_deadline_and_failure_policy() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    for label in ["budget-first", "budget-second"] {
        native(&fixture, &mut document, "tool.before", label);
    }
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(600);
    for step in document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
    {
        step["on_error"] = json!("report");
    }
    let service = Service::start(&fixture, &document);
    let output = fixture.run(launch(&fixture));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.root.join("budget-first-completed").exists());
    assert!(!fixture.root.join("budget-second-completed").exists());
    assert!(fixture.root.join("tool-executed").exists());
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn independent_calls_keep_native_concurrency_and_separate_event_identity() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    native(&fixture, &mut document, "tool.before", "barrier");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("FAKE_QWENPAW_PARALLEL", "1");
    let output = fixture.successful(command);
    assert_eq!(output.as_array().unwrap().len(), 2);
    assert!(output
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["tool_executed"] == true));
    for identifier in ["call-one", "call-two"] {
        assert!(fixture.root.join(format!("overlap-{identifier}")).exists());
    }
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn native_callback_environment_preserves_profile_changes_without_rebinding_provider() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    fixture.provider(&mut document);
    document["spec"]["providers"]["policy"]["transport"]["argv"] = json!([
        "/usr/bin/python3",
        action(),
        fixture.root,
        "provider",
        "policy"
    ]);
    native(&fixture, &mut document, "tool.before", "environment");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command
        .env("FAKE_QWENPAW_PROFILE", "1")
        .env("REMOVED_BY_NATIVE_HOST", "launch-value");
    assert_eq!(fixture.successful(command)["tool_executed"], true);
    let observed = |name| -> Value {
        serde_json::from_slice(&fs::read(fixture.root.join(name)).unwrap()).unwrap()
    };
    assert_eq!(
        observed("native-environment.json"),
        json!({
            "PROFILE_SECRET": "qwenpaw-callback-profile-secret", "REMOVED_BY_NATIVE_HOST": null,
        })
    );
    assert_eq!(
        observed("tool.before-provider-env.json"),
        json!({
            "PROFILE_SECRET": null, "REMOVED_BY_NATIVE_HOST": "launch-value",
        })
    );
    preserved(&fixture);
    service.released(&fixture);
}
