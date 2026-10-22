//! Editing one desired file never retargets the connected service or activates implicitly.

use super::{
    attachment::{action, install, native, success},
    support::{Fixture, Service},
};
use serde_json::{json, Value};
use std::{fs, process::Output};

fn update(fixture: &Fixture) -> Output {
    let mut command = fixture.command();
    command
        .args(["update", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder"]);
    fixture.run(command)
}

fn denial(document: &mut Value, label: &str) {
    document["spec"]["providers"][label] = json!({"protocol":"native-hook/v1alpha1","transport":{"type":"stdio","location":"agent","argv":["/bin/sh","-c","exit 2"]},"timeout_ms":500,"max_output_bytes":4096,"config":{}});
    document["spec"]["events"]["tool.before"] = json!({"enabled":true,"steps":[{"id":label,"provider":label,"native":{},"on_error":"block"}]});
}

#[test]
fn edited_connection_config_updates_explicitly_and_detaches_without_bootstrap_file() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Service::start(&fixture, &baseline);
    let (_native, pid) = native(&fixture);
    let attached = success(install(&fixture, pid, false));
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    let original = attached["policy_revision"].clone();
    let mut desired = baseline.clone();
    denial(&mut desired, "first");
    // Simulate an editor's atomic file replacement, preserving only the path.
    let next = fixture.root.join("editor.tmp");
    fs::write(&next, serde_json::to_vec(&desired).unwrap()).unwrap();
    fs::rename(next, fixture.root.join("aw.json")).unwrap();
    assert_eq!(
        service.status()["instances"][0]["policy_revision"],
        original
    );
    assert!(action(&fixture, 0, "turn")
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["code"] == 0));
    let applied = success(update(&fixture));
    assert_ne!(applied["policy_revision"], original);
    let active = service.status()["instances"][0]["policy_revision"].clone();
    assert!(action(&fixture, 1, "turn")
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["code"] == 2));
    action(&fixture, 2, "begin");
    fixture.save(&baseline);
    let busy = update(&fixture);
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));
    assert_eq!(service.status()["instances"][0]["policy_revision"], active);
    action(&fixture, 3, "end");
    // A changed endpoint is policy data, never permission to select another daemon.
    let mut invalid = baseline.clone();
    invalid["spec"]["daemon"]["state_dir"] = json!(fixture.root.join("another-service"));
    fixture.save(&invalid);
    let rejected = update(&fixture);
    assert!(!rejected.status.success());
    assert_eq!(service.status()["instances"][0]["policy_revision"], active);
    assert!(!fixture.root.join("another-service").exists());
    fixture.save(&baseline);
    success(update(&fixture));
    assert_eq!(
        service.status()["instances"][0]["policy_revision"],
        original
    );
    fixture.save(&desired);
    success(update(&fixture));
    let mut status = fixture.command();
    status
        .args(["service", "status", "--config"])
        .arg(fixture.root.join("aw.json"));
    assert_eq!(
        success(fixture.run(status))["result"]["instances"][0]["policy_revision"],
        active
    );
    success(install(&fixture, pid, true));
    assert_eq!(service.status()["bindings"], 0);
    // A later attach applies the same desired document before it returns.
    success(install(&fixture, pid, false));
    assert_eq!(service.status()["instances"][0]["policy_revision"], active);
    assert!(action(&fixture, 4, "turn")
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["code"] == 2));
    success(install(&fixture, pid, true));
    identity.check().unwrap();
}

fn receipt(fixture: &Fixture) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    let path = fixture.root.join("aw.json").canonicalize().unwrap();
    let key = format!("{:x}", Sha256::digest(path.as_os_str().as_bytes()));
    std::path::PathBuf::from(format!("/tmp/aw-{}/connections/{key}.json", unsafe {
        libc::geteuid()
    }))
}

#[test]
fn unrecorded_explicit_service_ignores_an_unused_runtime_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let _service = Service::start(&fixture, &fixture.document());
    let runtime = fixture.root.join("unused-runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    let mut status = fixture.command();
    status
        .args(["service", "status", "--config"])
        .arg(fixture.root.join("aw.json"))
        .env("XDG_RUNTIME_DIR", &runtime);
    success(fixture.run(status));
    assert!(!runtime.join("aw").exists());
}

#[test]
fn mutable_config_connection_rejects_stale_identity_and_unsafe_receipts() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    let service = Service::start(&fixture, &fixture.document());
    let (_native, pid) = native(&fixture);
    success(install(&fixture, pid, false));
    let path = receipt(&fixture);
    let original = fs::read(&path).unwrap();
    let baseline = service.status()["instances"][0]["policy_revision"].clone();
    let mut desired = fixture.document();
    denial(&mut desired, "deny");
    fixture.save(&desired);
    let mut stale: Value = serde_json::from_slice(&original).unwrap();
    stale["identity"]["generation"] = json!("stale");
    fs::write(&path, stale.to_string()).unwrap();
    let result = update(&fixture);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("recorded AW service changed"));
    assert_eq!(
        service.status()["instances"][0]["policy_revision"],
        baseline
    );
    fs::write(&path, &original).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!update(&fixture).status.success());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let directory = tempfile::tempdir_in(path.parent().unwrap()).unwrap();
    let backup = directory.path().join("receipt.json");
    fs::rename(&path, &backup).unwrap();
    symlink(&backup, &path).unwrap();
    assert!(!update(&fixture).status.success());
    fs::remove_file(&path).unwrap();
    fs::rename(backup, &path).unwrap();
    success(update(&fixture));
    success(install(&fixture, pid, true));
}
