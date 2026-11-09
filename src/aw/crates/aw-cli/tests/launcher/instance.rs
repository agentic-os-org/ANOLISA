//! Public instance commands retain native adoption and idle-update checks.

use super::super::{
    attachment::{action, native, success},
    support::{Fixture, Service},
};
use serde_json::json;
use std::fs;

#[test]
fn qoder_native_input_policy_updates_before_model_dispatch() {
    qoder_input_policy(false);
}

#[test]
fn qoder_native_submission_outlives_lifecycle_timeout_and_drains_denial() {
    qoder_input_policy(true);
}

fn qoder_input_policy(delayed: bool) {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    let service = Service::start(&fixture, &document);
    let config = fixture.root.join("aw.yaml");
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    let (_native, pid) = native(&fixture);
    let mut attach = fixture.command();
    attach.args([
        "instance",
        "attach",
        "--agent",
        "qoder",
        "--pid",
        &pid.to_string(),
    ]);
    success(fixture.run(attach));
    action(&fixture, 0, "turn");
    assert!(fixture.root.join("model-dispatch-0.json").exists());
    let provider = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(if delayed {
        "tests/fixtures/launcher/delayed_submission.py"
    } else {
        "tests/fixtures/pi/submission-policy.py"
    });
    document["spec"]["providers"]["input-deny"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":[fixture.python,
            provider,fixture.root]},
        "timeout_ms":if delayed { 6000 } else { 500 },"max_output_bytes":4096,"config":{}
    });
    document["spec"]["events"]["input.submit"] = json!({"enabled":true,"budget_ms":7000,
        "steps":[{"id":"input-deny","provider":"input-deny","operation":"submit",
            "effects":["observe","block"],"on_error":"block"}]});
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    let mut update = fixture.command();
    update.args(["instance", "update", "--agent", "qoder"]);
    success(fixture.run(update));
    assert!(action(&fixture, 1, "turn")
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["code"] == 2));
    assert!(!fixture.root.join("model-dispatch-1.json").exists());
    assert_eq!(service.status()["instances"][0]["requests"], 0);
    assert_eq!(service.status()["events"], 0);
    if delayed {
        assert!(fixture.root.join("delayed-submission-finished").exists());
    }
    document["spec"]["events"]["input.submit"]["steps"] = json!([]);
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    let mut update = fixture.command();
    update.args(["instance", "update", "--agent", "qoder"]);
    success(fixture.run(update));
    action(&fixture, 2, "turn");
    assert!(fixture.root.join("model-dispatch-2.json").exists());
    let mut detach = fixture.command();
    detach.args([
        "instance",
        "attach",
        "--agent",
        "qoder",
        "--pid",
        &pid.to_string(),
        "--remove",
    ]);
    success(fixture.run(detach));
    assert_eq!(service.status()["bindings"], 0);
    fs::write(
        fixture.root.join("attach-command-3.json"),
        br#"{"action":"stop"}"#,
    )
    .unwrap();
}

#[test]
fn qoder_instance_uses_one_default_configuration_and_preserves_native_process() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"]["deny"] = json!({
        "protocol":"native-hook/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":["/bin/sh","-c","exit 2"]},
        "timeout_ms":500,"max_output_bytes":4096,"config":{}
    });
    document["spec"]["events"]["tool.before"] = json!({
        "enabled":true,"steps":[{"id":"deny","provider":"deny","native":{},"on_error":"block"}]
    });
    let service = Service::start(&fixture, &document);
    let config = fixture.root.join("aw.yaml");
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    let (_native, pid) = native(&fixture);
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    let mut attach = fixture.command();
    attach.args([
        "instance",
        "attach",
        "--agent",
        "qoder",
        "--pid",
        &pid.to_string(),
    ]);
    let installed = success(fixture.run(attach));
    assert_eq!(installed["attached"], true);
    let instance = service.status()["instances"][0]["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let revision = service.status()["instances"][0]["policy_revision"].clone();
    assert!(action(&fixture, 0, "turn")
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["code"] == 2));

    document["spec"]["events"]["tool.before"]["steps"] = json!([]);
    fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    action(&fixture, 1, "begin");
    let mut update = fixture.command();
    update.args(["instance", "update", "--instance", &instance]);
    let rejected = fixture.run(update);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("busy"));
    assert_eq!(
        service.status()["instances"][0]["policy_revision"],
        revision
    );
    action(&fixture, 2, "end");
    let mut update = fixture.command();
    update.args(["instance", "update", "--instance", &instance]);
    let updated = success(fixture.run(update));
    assert_ne!(updated["policy_revision"], revision);
    assert!(action(&fixture, 3, "turn")
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["code"] == 0));
    identity.check().unwrap();

    let mut remove = fixture.command();
    remove.args([
        "instance",
        "attach",
        "--agent",
        "qoder",
        "--pid",
        &pid.to_string(),
        "--remove",
    ]);
    success(fixture.run(remove));
    assert_eq!(service.status()["bindings"], 0);
    assert_eq!(action(&fixture, 4, "turn"), json!([]));
    identity.check().unwrap();
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("home/.qoder/settings.json")).unwrap())
            .unwrap();
    assert_eq!(settings["theme"], "keep");
}
