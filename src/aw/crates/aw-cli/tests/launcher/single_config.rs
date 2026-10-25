//! Editing one desired file never retargets the connected service or activates implicitly.

use super::{
    attachment::{action, install, native, success},
    support::{Fixture, Process, Service, TIMEOUT},
};
use serde_json::{json, Value};
use std::{fs, os::unix::process::ExitStatusExt, process::Output};

#[test]
fn configured_shutdown_grace_reaches_the_foreground_launcher() {
    for configured in [false, true] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        if configured {
            // A schema integer may arrive as a floating-point JSON number.
            document["spec"]["agents"]["qoder"]["shutdown_grace_ms"] = json!(4000.0);
        }
        let service = Service::start(&fixture, &document);
        let mut process = Process::spawn(
            &fixture,
            fixture.launch(&["--hold", "--shutdown-delay", "3"], None),
        );
        process
            .wait_until("shutdown handler ready", TIMEOUT, |_| {
                Ok((
                    fixture.root.join("ready-one").exists(),
                    "waiting for ready marker".into(),
                ))
            })
            .unwrap();
        let pid = fs::read_to_string(fixture.root.join("started-one")).unwrap();
        process.signal(libc::SIGTERM);
        let output = process.finish();
        assert_eq!(fixture.root.join("cleanup-one").exists(), configured);
        if configured {
            assert_eq!(
                output.status.code(),
                Some(37),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            assert_eq!(output.status.signal(), Some(libc::SIGKILL));
        }
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        service.released(&fixture);
    }
}

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
fn ordinary_launch_preserves_auto_connection_after_detach_and_refreshes_after_stop() {
    use aw_service::{Client, Operation};
    use std::time::{Duration, Instant};
    let fixture = Fixture::new();
    // A private short runtime path keeps auto Unix sockets within sockaddr_un.
    let runtime = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(runtime.path().join("aw")).unwrap();
    fs::set_permissions(runtime.path().join("aw"), fs::Permissions::from_mode(0o700)).unwrap();
    let mut baseline = fixture.document();
    baseline["spec"]["daemon"] =
        json!({"startup":"on_demand","endpoint":"auto","state_dir":"auto"});
    fixture.save(&baseline);
    let resolve = || {
        aw_service::launch_service::resolve(
            &fs::read(fixture.root.join("aw.json")).unwrap(),
            Some(runtime.path()),
        )
        .unwrap()
    };
    let original = resolve();
    let service = Service::start_at(&fixture, &baseline, &original.state_dir);
    let (native_process, pid) = native(&fixture);
    let connect = |remove| {
        let mut command = fixture.command();
        command
            .args(["agent", "connect", "--config"])
            .arg(fixture.root.join("aw.json"))
            .args(["--agent", "qoder", "--pid", &pid.to_string()])
            .env("XDG_RUNTIME_DIR", runtime.path());
        if remove {
            command.arg("--remove");
        }
        success(fixture.run(command))
    };
    connect(false);
    connect(true);
    assert_eq!(service.status()["bindings"], 0);
    drop(native_process);
    fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/qoder.py"),
        fixture.root.join("fake qoder"),
    )
    .unwrap();
    fs::set_permissions(
        fixture.root.join("fake qoder"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut desired = baseline.clone();
    denial(&mut desired, "deny");
    fixture.save(&desired);
    let edited = resolve();
    assert!(!edited.state_dir.exists());
    let mut command = fixture.launch(&[], None);
    command.env("XDG_RUNTIME_DIR", runtime.path());
    let rejected = fixture.run(command);
    // Teardown also covers the unguarded implementation used for the negative run.
    let created = edited.state_dir.exists();
    if created {
        let client = Client::connect(&edited.socket, Instant::now() + TIMEOUT).unwrap();
        client
            .call(Operation::Stop, Instant::now() + TIMEOUT)
            .unwrap();
    }
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("configuration path already selects another service"),
        "{rejected:?}"
    );
    assert!(!created, "ordinary launch started an unrecorded daemon");
    drop(service);
    assert!(!original.socket.exists());
    // A direct service shutdown leaves the path receipt behind intentionally.
    let mut command = fixture.command();
    command
        .args(["serve", "--config"])
        .arg(fixture.root.join("aw.json"))
        .arg("--state-dir")
        .arg(&edited.state_dir);
    let mut service = Process::spawn(&fixture, command);
    service
        .wait_until("replacement service", TIMEOUT, |_| {
            Ok((
                Client::connect(&edited.socket, Instant::now() + Duration::from_millis(100))
                    .is_ok(),
                "waiting for socket".into(),
            ))
        })
        .unwrap();
    let client = Client::connect(&edited.socket, Instant::now() + TIMEOUT).unwrap();
    let mut command = fixture.launch(&["--hold"], None);
    command.env("XDG_RUNTIME_DIR", runtime.path());
    let mut run = Process::spawn(&fixture, command);
    run.wait_until("ordinary Agent readiness", TIMEOUT, |_| {
        Ok((
            fixture.root.join("ready-one").exists(),
            "waiting for Agent".into(),
        ))
    })
    .unwrap();
    let mut status = fixture.command();
    status
        .args(["service", "status", "--config"])
        .arg(fixture.root.join("aw.json"))
        .env("XDG_RUNTIME_DIR", runtime.path());
    let status = success(fixture.run(status));
    assert_eq!(
        status["identity"],
        serde_json::to_value(client.identity()).unwrap()
    );
    assert_eq!(status["result"]["bindings"], 1);
    fs::write(fixture.root.join("release-one"), b"").unwrap();
    assert!(run.finish().status.success());
    let mut stop = fixture.command();
    stop.args(["service", "stop", "--config"])
        .arg(fixture.root.join("aw.json"))
        .env("XDG_RUNTIME_DIR", runtime.path());
    success(fixture.run(stop));
    assert!(service.finish().status.success());
    assert!(!edited.socket.exists());
}

#[test]
fn edited_reloadable_launch_rejects_connection_before_starting_another_daemon() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Service::start(&fixture, &baseline);
    let (_native, pid) = native(&fixture);
    success(install(&fixture, pid, false));
    let mut desired = baseline.clone();
    desired["spec"]["daemon"] = json!({"startup":"on_demand","endpoint":"auto","state_dir":"auto"});
    fixture.save(&desired);
    let bytes = fs::read(fixture.root.join("aw.json")).unwrap();
    let paths = aw_service::launch_service::resolve(&bytes, None).unwrap();
    assert!(!paths.state_dir.exists());
    let mut command = fixture.command();
    command
        .args(["run", "--reloadable", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder"]);
    let result = fixture.run(command);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("configuration path already selects another service"));
    assert!(
        !paths.state_dir.exists(),
        "rejected launch created an unrecorded service"
    );
    assert_eq!(service.status()["bindings"], 1);
    fixture.save(&baseline);
    success(install(&fixture, pid, true));
}

#[test]
fn rejected_reattach_confirms_qoder_unload_before_deleting_bridge() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Service::start(&fixture, &baseline);
    let (_native, pid) = native(&fixture);
    success(install(&fixture, pid, false));
    success(install(&fixture, pid, true));
    let previous: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("native-detach.json")).unwrap())
            .unwrap();
    fs::remove_file(fixture.root.join("native-detach.json")).unwrap();
    let mut invalid = baseline.clone();
    invalid["spec"]["daemon"]["state_dir"] = json!(fixture.root.join("other-service"));
    fixture.save(&invalid);
    let result = install(&fixture, pid, false);
    assert!(!result.status.success());
    let confirmed: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("native-detach.json")).unwrap())
            .unwrap();
    assert_eq!(confirmed["code"], 0);
    assert_ne!(confirmed["descriptor"], previous["descriptor"]);
    assert_eq!(service.status()["bindings"], 0);
    assert!(!fixture
        .root
        .join("home/.qoder/.aw-attachment.json")
        .exists());
    assert_eq!(action(&fixture, 0, "turn"), json!([]));
    fixture.save(&baseline);
    success(install(&fixture, pid, false));
    success(install(&fixture, pid, true));
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
    let mut run = fixture.launch(&[], None);
    run.env("XDG_RUNTIME_DIR", &runtime);
    success(fixture.run(run));
    assert!(!receipt(&fixture).exists());
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
