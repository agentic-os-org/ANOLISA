//! Public instance commands retain native adoption and idle-update checks.

use super::super::{
    attachment::{action, native, success},
    support::{Fixture, Service},
};
use serde_json::json;
use std::fs;

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
