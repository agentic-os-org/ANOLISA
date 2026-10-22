//! Native watcher fixtures exercise real install/remove commands and service admission.

use super::support::{Fixture, Process, Service, TIMEOUT};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

fn wait(path: &std::path::Path) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        thread::sleep(Duration::from_millis(5));
    }
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn native_command(fixture: &Fixture) -> Command {
    // A parallel fork can briefly retain a script writer until exec; invoke
    // Python directly so fixture startup does not depend on ETXTBSY timing.
    let mut command = Command::new(&fixture.python);
    command.arg(fixture.root.join("fake qoder"));
    command
}

fn native(fixture: &Fixture) -> (Process, u32) {
    let scope = fixture.root.join("home/.qoder");
    fs::create_dir_all(&scope).unwrap();
    fs::set_permissions(&scope, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(scope.join("settings.json"), b"{\"theme\":\"keep\"}").unwrap();
    fs::set_permissions(
        scope.join("settings.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launcher/attach_host.py");
    fs::copy(source, fixture.root.join("fake qoder")).unwrap();
    fs::set_permissions(
        fixture.root.join("fake qoder"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut command = native_command(fixture);
    command
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root);
    let process = Process::spawn(fixture, command);
    let pid = wait(&fixture.root.join("native-pid.json"))
        .as_u64()
        .unwrap() as u32;
    (process, pid)
}

fn install(fixture: &Fixture, pid: u32, remove: bool) -> Output {
    install_entrypoint(fixture, pid, remove, &["install"])
}

fn install_entrypoint(fixture: &Fixture, pid: u32, remove: bool, entry: &[&str]) -> Output {
    let mut command = fixture.command();
    command
        .args(entry)
        .arg("--config")
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--pid"])
        .arg(pid.to_string());
    if remove {
        command.arg("--remove");
    }
    fixture.run(command)
}

fn action(fixture: &Fixture, index: usize, action: &str) -> Value {
    let temporary = fixture.root.join("attach-command.tmp");
    fs::write(&temporary, json!({"action":action}).to_string()).unwrap();
    fs::rename(
        temporary,
        fixture.root.join(format!("attach-command-{index}.json")),
    )
    .unwrap();
    wait(&fixture.root.join(format!("attach-result-{index}.json")))
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn external_install_first_hook_busy_detach_isolation_and_owned_cleanup() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"]["deny"] = json!({"protocol":"native-hook/v1alpha1","transport":{"type":"stdio","location":"agent","argv":["/bin/sh","-c","exit 2"]},"timeout_ms":500,"max_output_bytes":4096,"config":{}});
    document["spec"]["events"]["tool.before"] = json!({"enabled":true,"steps":[{"id":"deny","provider":"deny","native":{},"on_error":"block"}]});
    let service = Service::start(&fixture, &document);
    let (_process, pid) = native(&fixture);
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    let other = Fixture::new();
    let (_other_process, other_pid) = native(&other);
    let untouched = fs::read(other.root.join("home/.qoder/settings.json")).unwrap();
    let installed = success(install(&fixture, pid, false));
    assert_eq!(installed["attached"], true);
    assert!(!install(&fixture, pid, false).status.success());
    let first = action(&fixture, 0, "turn");
    assert!(
        first
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["code"] == 2),
        "{first}"
    );
    assert_eq!(action(&other, 0, "turn"), json!([]));
    assert_eq!(
        fs::read(other.root.join("home/.qoder/settings.json")).unwrap(),
        untouched
    );
    aw_service::ProcessIdentity::read(other_pid)
        .unwrap()
        .check()
        .unwrap();
    action(&fixture, 1, "begin");
    let busy = install(&fixture, pid, true);
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));
    action(&fixture, 2, "end");
    let settings = fixture.root.join("home/.qoder/settings.json");
    let mut edited: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    edited["theme"] = json!("user-edited");
    edited["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(json!({"matcher":"unused","hooks":[{"type":"command","command":"/bin/true"}]}));
    fs::write(&settings, edited.to_string()).unwrap();
    success(install(&fixture, pid, true));
    assert_eq!(service.status()["bindings"], 0);
    let cleaned: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert_eq!(cleaned["theme"], "user-edited");
    assert_eq!(cleaned["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert!(!fixture
        .root
        .join("home/.qoder/.aw-attachment.json")
        .exists());
    assert!(!fs::read_dir(fixture.root.join("state"))
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("attach-")));
    assert_eq!(action(&fixture, 3, "turn"), json!([]));
    success(install(&fixture, pid, false));
    success(install(&fixture, pid, true));
    identity.check().unwrap();
}

#[test]
fn external_install_rejects_wrong_pid_and_shared_profile_without_writes() {
    let fixture = Fixture::new();
    let document = fixture.document();
    fixture.save(&document);
    let (_process, pid) = native(&fixture);
    let settings = fixture.root.join("home/.qoder/settings.json");
    let original = fs::read(&settings).unwrap();
    let wrong = install(&fixture, std::process::id(), false);
    assert!(!wrong.status.success());
    let mut command = native_command(&fixture);
    command
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root);
    let _shared = Process::spawn(&fixture, command);
    thread::sleep(Duration::from_millis(100));
    let shared = install(&fixture, pid, false);
    assert!(!shared.status.success());
    assert!(String::from_utf8_lossy(&shared.stderr).contains("shared"));
    assert_eq!(fs::read(settings).unwrap(), original);
    assert!(!fixture
        .root
        .join("home/.qoder/.aw-attachment.json")
        .exists());
}

#[test]
fn cancelled_unconfirmed_install_rolls_back_native_groups_and_binding() {
    let fixture = Fixture::new();
    let document = fixture.document();
    let service = Service::start(&fixture, &document);
    // Prepare the native fixture, then start its unconfirmed counterpart.
    let (process, _) = native(&fixture);
    drop(process);
    fs::remove_file(fixture.root.join("native-pid.json")).unwrap();
    let mut command = native_command(&fixture);
    command
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root)
        .env("FAKE_SKIP_CONFIRM", "1");
    let _native = Process::spawn(&fixture, command);
    let pid = wait(&fixture.root.join("native-pid.json"))
        .as_u64()
        .unwrap() as u32;
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    let mut command = fixture.command();
    command
        .args(["install", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--pid"])
        .arg(pid.to_string());
    let installer = Process::spawn(&fixture, command);
    let deadline = Instant::now() + TIMEOUT;
    while service.status()["bindings"] != 1 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    installer.signal(libc::SIGTERM);
    assert!(!installer.finish().status.success());
    assert_eq!(service.status()["bindings"], 0);
    identity.check().unwrap();
    assert!(!fixture
        .root
        .join("home/.qoder/.aw-attachment.json")
        .exists());
    let settings: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("home/.qoder/settings.json")).unwrap())
            .unwrap();
    assert_eq!(settings, json!({"theme":"keep"}));
}

#[test]
fn external_install_creates_missing_native_settings_without_launching_an_agent() {
    let fixture = Fixture::new();
    let document = fixture.document();
    let service = Service::start(&fixture, &document);
    let (_process, pid) = native(&fixture);
    let settings = fixture.root.join("home/.qoder/settings.json");
    fs::remove_file(&settings).unwrap();
    success(install_entrypoint(
        &fixture,
        pid,
        false,
        &["agent", "connect"],
    ));
    success(install_entrypoint(
        &fixture,
        pid,
        true,
        &["agent", "connect"],
    ));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(settings).unwrap()).unwrap(),
        json!({})
    );
    assert_eq!(service.status()["bindings"], 0);
    aw_service::ProcessIdentity::read(pid)
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn external_qwenpaw_api_loads_private_descriptor_and_preserves_busy_protection() {
    let fixture = Fixture::new();
    let profile = fixture.root.join("home");
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("config.json"), b"{}").unwrap();
    fs::create_dir_all(profile.join("plugins/unrelated")).unwrap();
    fs::write(profile.join("plugins/unrelated/owned-by-user"), b"preserve").unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launcher/attach_app.py");
    fs::copy(source, fixture.root.join("fake qoder")).unwrap();
    fs::set_permissions(
        fixture.root.join("fake qoder"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut document = fixture.document();
    document["spec"]["agents"]["qoder"]["adapter"] = json!("qwenpaw");
    document["spec"]["agents"]["qoder"]["argv"] = json!([
        fixture.root.join("fake qoder"),
        "app",
        "--host",
        "127.0.0.1",
        "--port",
        port.to_string()
    ]);
    let service = Service::start(&fixture, &document);
    let mut command = native_command(&fixture);
    command
        .current_dir(&fixture.root)
        .args(["app", "--host", "127.0.0.1", "--port"])
        .arg(port.to_string())
        .env("QWENPAW_WORKING_DIR", &profile)
        .env("FAKE_ROOT", &fixture.root)
        .env_remove("AW_NATIVE_CONFIG");
    let _native = Process::spawn(&fixture, command);
    let pid = wait(&fixture.root.join("native-pid.json"))
        .as_u64()
        .unwrap() as u32;
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    success(install(&fixture, pid, false));
    assert!(profile
        .join("plugins/aw-native/aw-attachment.json")
        .is_file());
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    client
        .post(format!("http://127.0.0.1:{port}/begin"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    let busy = install(&fixture, pid, true);
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));
    client
        .post(format!("http://127.0.0.1:{port}/end"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    let plugin = profile.join("plugins/aw-native");
    let moved = profile.join("plugins/moved-aw-native");
    fs::rename(&plugin, &moved).unwrap();
    let missing = install(&fixture, pid, true);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("unload cannot be verified"));
    let record: Value =
        serde_json::from_slice(&fs::read(profile.join(".aw-attachment.json")).unwrap()).unwrap();
    assert!(std::path::Path::new(record["binding"].as_str().unwrap()).is_file());
    fs::rename(moved, &plugin).unwrap();
    success(install(&fixture, pid, true));
    assert_eq!(service.status()["bindings"], 0);
    assert!(!profile.join("plugins/aw-native").exists());
    assert!(!profile.join(".aw-attachment.json").exists());
    assert_eq!(
        fs::read(profile.join("plugins/unrelated/owned-by-user")).unwrap(),
        b"preserve"
    );
    identity.check().unwrap();
}

#[test]
fn native_fixture_starts_while_a_script_writer_is_open() {
    let fixture = Fixture::new();
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(fixture.root.join("fake qoder"))
        .unwrap();
    let (_process, pid) = native(&fixture);
    aw_service::ProcessIdentity::read(pid)
        .unwrap()
        .check()
        .unwrap();
    drop(writer);
}
