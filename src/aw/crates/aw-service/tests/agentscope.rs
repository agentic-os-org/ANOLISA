//! SDK launcher rejection tests require no third-party Python dependency.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use support::{Fixture, Service};

fn setup(fixture: &Fixture, version: &str) -> Value {
    let host = fixture.root.join("python-sdk-probe");
    // Deliberately claim a version without attaching a Bridge. A version probe
    // alone must never be accepted as native readiness or effect adoption.
    fs::write(
        &host,
        format!("#!/usr/bin/python3\nimport sys\nif sys.argv[1] == '-c': print('{version}')\n"),
    )
    .unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"sdk": {"adapter": "agentscope", "argv": [host, "worker.py"]}});
    document
}

fn launch(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .args(["run", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "sdk"]);
    command
}

#[test]
fn unsupported_sdk_version_and_interpreter_modes_fail_before_creating_service() {
    let fixture = Fixture::new();
    fixture.save(&setup(&fixture, "2.0.8"));
    let output = fixture.run(launch(&fixture));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("2.0.6 is required"));
    assert!(!fixture.root.join("state").exists());
    for args in [
        json!(["-I", "worker.py"]),
        json!(["-c", "pass"]),
        json!(["-m"]),
    ] {
        let mut document = setup(&fixture, "2.0.6");
        document["spec"]["agents"]["sdk"]["argv"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        document["spec"]["agents"]["sdk"]["argv"]
            .as_array_mut()
            .unwrap()
            .extend(args.as_array().unwrap().iter().cloned());
        fixture.save(&document);
        assert!(!fixture.run(launch(&fixture)).status.success());
        assert!(!fixture.root.join("state").exists());
    }
}

#[test]
fn no_factory_attachment_never_reports_ready_and_releases_binding_files() {
    let fixture = Fixture::new();
    let document = setup(&fixture, "2.0.6");
    let service = Service::start(&fixture, &document);
    let output = fixture.run(launch(&fixture));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("before native hooks became ready"));
    service.released(&fixture);
}
