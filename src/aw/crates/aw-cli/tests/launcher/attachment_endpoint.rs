//! Binding preparation must not turn a port replacement into plugin installation.

use super::{
    attachment::{app_command, wait, QWENPAW_PROCESSES},
    support::{Fixture, Process, Service, TIMEOUT},
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn app(fixture: &Fixture, port: u16, blocked: bool) -> (Process, u32) {
    let profile = fixture.root.join("home");
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("config.json"), "{}").unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/attach_app.py");
    fs::copy(source, fixture.root.join("fake qwenpaw")).unwrap();
    fs::set_permissions(
        fixture.root.join("fake qwenpaw"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut command = app_command(fixture);
    command
        .current_dir(&fixture.root)
        .args(["app", "--host", "127.0.0.1", "--port"])
        .arg(port.to_string())
        .env("QWENPAW_WORKING_DIR", profile)
        .env("FAKE_ROOT", &fixture.root)
        .env("FAKE_PREPARE_WAIT", if blocked { "1" } else { "0" });
    let process = Process::spawn(fixture, command);
    let pid = wait(&fixture.root.join("native-pid.json"))
        .as_u64()
        .unwrap() as u32;
    (process, pid)
}

#[test]
fn qwenpaw_port_replaced_during_bind_never_receives_plugin_install() {
    let _apps = QWENPAW_PROCESSES.lock().unwrap();
    let fixture = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut document = fixture.document();
    document["spec"]["agents"]["qoder"] = json!({"adapter":"qwenpaw","argv":[fixture.root.join("fake qwenpaw"),"app","--host","127.0.0.1","--port",port.to_string()]});
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    let (_native, pid) = app(&fixture, port, true);
    let mut command = fixture.command();
    command
        .args(["agent", "connect", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--pid"])
        .arg(pid.to_string());
    let attaching = Process::spawn(&fixture, command);
    let deadline = Instant::now() + TIMEOUT;
    while !fixture.root.join("preparing.pid").exists() {
        let error = fs::read_to_string(fixture.root.join("process-2.stderr")).unwrap_or_default();
        assert!(error.is_empty(), "{error}");
        assert!(
            Instant::now() < deadline,
            "provider preparation did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fs::write(fixture.root.join("close-listener"), "").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !fixture.root.join("listener-closed").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let other = Fixture::new();
    let (_replacement, replacement_pid) = app(&other, port, false);
    fs::write(fixture.root.join("allow-prepare"), "").unwrap();
    let output = attaching.finish();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("endpoint is not owned"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.root.join("post-seen").exists());
    assert!(!other.root.join("post-seen").exists());
    assert!(!other.root.join("home/plugins/aw-native").exists());
    assert_eq!(service.status()["bindings"], 0);
    aw_service::ProcessIdentity::read(replacement_pid)
        .unwrap()
        .check()
        .unwrap();
}
