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
        format!("#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\nif sys.argv[1] == '-c': Path(sys.argv[3]).write_text('{version}')\n"),
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
fn invalid_sdk_metadata_and_interpreter_modes_fail_before_creating_service() {
    let fixture = Fixture::new();
    fixture.save(&setup(&fixture, "invalid version"));
    let output = fixture.run(launch(&fixture));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must report a version"));
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
    for version in ["2.0.5", "2.0.6", "2.0.8", "3.0.0rc1"] {
        let fixture = Fixture::new();
        let document = setup(&fixture, version);
        let service = Service::start(&fixture, &document);
        let output = fixture.run(launch(&fixture));
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("before native hooks became ready")
        );
        service.released(&fixture);
    }
}

#[test]
fn submission_boundary_cleans_turns_and_preserves_failure_policy() {
    let fixture = Fixture::new();
    let mut command = Command::new(&fixture.python);
    command.arg(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/agentscope/submission_unit.py"
    ));
    let result = fixture.run(command);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn version_probe_accepts_slow_noisy_python_startup() {
    let fixture = Fixture::new();
    let modules = fixture.root.join("modules");
    let metadata = modules.join("agentscope-2.0.6.dist-info");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(
        metadata.join("METADATA"),
        "Name: agentscope\nVersion: 2.0.6\n",
    )
    .unwrap();
    fs::write(modules.join("sitecustomize.py"), "import sys, time\nif sys.argv[0] == '-c':\n    print('instrumentation startup banner', flush=True)\n    time.sleep(6)\n").unwrap();
    fs::write(fixture.root.join("worker.py"), "raise SystemExit(23)\n").unwrap();
    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"sdk": {"adapter": "agentscope", "argv": [fixture.python, "worker.py"]}});
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("PYTHONPATH", modules);
    let output = support::Process::spawn(&fixture, command)
        .finish_with_timeout(std::time::Duration::from_secs(25));
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    service.released(&fixture);
}
