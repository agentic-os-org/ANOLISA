//! Native watcher fixtures exercise real install/remove commands and service admission.

#[path = "attachment_profiles.rs"]
mod profiles;

use super::support::{Fixture, Process, Service, TIMEOUT};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

// Unknown native peers deliberately block all attachment in this namespace.
pub(super) static QWENPAW_PROCESSES: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn app_command(fixture: &Fixture) -> Command {
    let mut command = Command::new(&fixture.python);
    command.arg(fixture.root.join("fake qwenpaw"));
    command
}

pub(super) fn wait(path: &std::path::Path) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        thread::sleep(Duration::from_millis(5));
    }
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

pub(super) fn native_command(fixture: &Fixture) -> Command {
    // A parallel fork can briefly retain a script writer until exec; invoke
    // Python directly so fixture startup does not depend on ETXTBSY timing.
    let mut command = Command::new(&fixture.python);
    command.arg(fixture.root.join("fake qoder"));
    command
}

pub(super) fn native(fixture: &Fixture) -> (Process, u32) {
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

pub(super) fn install(fixture: &Fixture, pid: u32, remove: bool) -> Output {
    install_entrypoint(fixture, pid, remove, &["install"])
}

pub(super) fn install_entrypoint(
    fixture: &Fixture,
    pid: u32,
    remove: bool,
    entry: &[&str],
) -> Output {
    let mut command = fixture.command();
    command
        .env("XDG_RUNTIME_DIR", "")
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

pub(super) fn action(fixture: &Fixture, index: usize, action: &str) -> Value {
    let temporary = fixture.root.join("attach-command.tmp");
    fs::write(&temporary, json!({"action":action}).to_string()).unwrap();
    fs::rename(
        temporary,
        fixture.root.join(format!("attach-command-{index}.json")),
    )
    .unwrap();
    wait(&fixture.root.join(format!("attach-result-{index}.json")))
}

pub(super) fn success(output: Output) -> Value {
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
    let other = fixture.root.join("other-installation");
    fs::create_dir(&other).unwrap();
    // Copy in a separate process so parallel test forks cannot inherit the
    // executable's writer and keep it ETXTBSY after the copy returns.
    let mut copy = Command::new(&fixture.python);
    copy.args(["-c", "import shutil,sys; shutil.copy2(*sys.argv[1:])"])
        .arg(&fixture.python)
        .arg(other.join("python3"));
    let copied = Process::spawn(&fixture, copy).finish();
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    fs::copy(fixture.root.join("fake qoder"), other.join("fake qoder")).unwrap();
    let mut command = Command::new(other.join("python3"));
    command
        .arg(other.join("fake qoder"))
        .current_dir(&other)
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
fn cancelled_unconfirmed_install_retains_bridge_until_confirmed_cleanup() {
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
    while !fs::read_to_string(fixture.root.join("home/.qoder/settings.json"))
        .unwrap()
        .contains("ConfigChange")
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    installer.signal(libc::SIGTERM);
    let result = installer.finish();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("attachment cleanup incomplete"));
    assert_eq!(service.status()["bindings"], 0);
    identity.check().unwrap();
    let ownership = fixture.root.join("home/.qoder/.aw-attachment.json");
    let retained: Value = serde_json::from_slice(&fs::read(&ownership).unwrap()).unwrap();
    assert!(std::path::Path::new(retained["binding"].as_str().unwrap()).is_file());
    fs::write(fixture.root.join("resume-confirmation"), b"resume").unwrap();
    success(install(&fixture, pid, true));
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
    let _apps = QWENPAW_PROCESSES.lock().unwrap();
    let fixture = Fixture::new();
    let profile = fixture.root.join("home");
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("config.json"), b"{}").unwrap();
    fs::create_dir_all(profile.join("plugins/unrelated")).unwrap();
    fs::write(profile.join("plugins/unrelated/owned-by-user"), b"preserve").unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launcher/attach_app.py");
    fs::copy(source, fixture.root.join("fake qwenpaw")).unwrap();
    fs::set_permissions(
        fixture.root.join("fake qwenpaw"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut document = fixture.document();
    document["spec"]["agents"]["qoder"]["adapter"] = json!("qwenpaw");
    document["spec"]["agents"]["qoder"]["argv"] = json!([
        fixture.root.join("fake qwenpaw"),
        "app",
        "--host",
        "127.0.0.1",
        "--port",
        port.to_string()
    ]);
    let service = Service::start(&fixture, &document);
    let mut command = app_command(&fixture);
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
    let ownership = profile.join(".aw-attachment.json");
    for (index, prefix) in [
        vec!["-m", "qwenpaw"],
        vec!["-u", "-m", "qwenpaw"],
        vec!["-uB", "-m", "qwenpaw"],
        vec!["-W", "ignore", "-m", "qwenpaw"],
        vec!["-X", "utf8", "-m", "qwenpaw"],
        vec!["-umqwenpaw"],
        vec!["-u", "qwenpaw.py"],
        vec!["-W", "ignore", "qwenpaw.py"],
        vec!["--", "qwenpaw.py"],
    ]
    .into_iter()
    .enumerate()
    {
        let peer_root = fixture.root.join(format!("module-peer-{index}"));
        fs::create_dir(&peer_root).unwrap();
        fs::copy(
            fixture.root.join("fake qwenpaw"),
            peer_root.join("qwenpaw.py"),
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut command = Command::new(&fixture.python);
        command
            .args(prefix)
            .args(["app", "--host", "127.0.0.1", "--port"])
            .arg(peer_port.to_string())
            .current_dir(&peer_root)
            .env("QWENPAW_WORKING_DIR", &profile)
            .env("FAKE_ROOT", &peer_root)
            .env_remove("AW_NATIVE_CONFIG");
        let peer = Process::spawn(&fixture, command);
        let peer_pid = wait(&peer_root.join("native-pid.json")).as_u64().unwrap() as u32;
        let shared = install_entrypoint(&fixture, pid, false, &["agent", "connect"]);
        assert!(!shared.status.success(), "{:?}", shared);
        assert!(String::from_utf8_lossy(&shared.stderr).contains("scope is shared"));
        assert_eq!(service.status()["bindings"], 0);
        assert!(!ownership.exists());
        assert!(!profile.join("plugins/aw-native").exists());
        assert!(!fixture.root.join("post-seen").exists());
        aw_service::ProcessIdentity::read(peer_pid)
            .unwrap()
            .check()
            .unwrap();
        identity.check().unwrap();
        drop(peer);
    }
    let fail_install = fixture.root.join("fail-install");
    let fail_status = fixture.root.join("fail-status");
    fs::write(&fail_install, b"").unwrap();
    let failed = install(&fixture, pid, false);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("503"));
    assert!(!ownership.exists());
    assert_eq!(service.status()["bindings"], 0);
    fs::write(&fail_status, b"").unwrap();
    let uncertain = install(&fixture, pid, false);
    assert!(!uncertain.status.success());
    assert!(String::from_utf8_lossy(&uncertain.stderr).contains("cleanup incomplete"));
    let record: Value = serde_json::from_slice(&fs::read(&ownership).unwrap()).unwrap();
    let staged = std::path::Path::new(record["registration"]["source"].as_str().unwrap());
    assert!(staged.join("plugin.py").is_file());
    assert!(staged.join("aw-attachment.json").is_file());
    assert!(!install(&fixture, pid, true).status.success());
    assert!(ownership.is_file());
    assert!(staged.join("plugin.py").is_file());
    fs::remove_file(fail_status).unwrap();
    success(install(&fixture, pid, true));
    assert!(!ownership.exists());
    assert!(!staged.exists());
    assert_eq!(service.status()["bindings"], 0);
    fs::remove_file(fail_install).unwrap();
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

#[test]
fn qoder_detach_retries_after_native_confirmation_timeout() {
    let fixture = Fixture::new();
    let service = Service::start(&fixture, &fixture.document());
    let (process, pid) = native(&fixture);
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    success(install(&fixture, pid, false));
    process.signal(libc::SIGSTOP);
    let timed_out = install(&fixture, pid, true);
    process.signal(libc::SIGCONT);
    assert!(!timed_out.status.success());
    assert!(
        String::from_utf8_lossy(&timed_out.stderr).contains("Qoder detach confirmation timed out")
    );
    let ownership = fixture.root.join("home/.qoder/.aw-attachment.json");
    let record: Value = serde_json::from_slice(&fs::read(&ownership).unwrap()).unwrap();
    let artifacts = std::path::Path::new(record["artifacts"].as_str().unwrap());
    assert!(artifacts.join("binding.json").is_file());
    success(install(&fixture, pid, true));
    assert!(!ownership.exists());
    assert!(!artifacts.exists());
    assert_eq!(service.status()["bindings"], 0);
    let settings: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("home/.qoder/settings.json")).unwrap())
            .unwrap();
    assert_eq!(settings, json!({"theme":"keep"}));
    success(install(&fixture, pid, false));
    success(install(&fixture, pid, true));
    identity.check().unwrap();
}

#[test]
fn empty_runtime_directory_allows_auto_path_detach() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["daemon"]["state_dir"] = json!("auto");
    fixture.save(&document);
    let bytes = fs::read(fixture.root.join("aw.json")).unwrap();
    let paths = aw_service::launch_service::resolve(&bytes, None).unwrap();
    let namespace = paths.state_dir.parent().unwrap();
    let created_namespace = !namespace.exists();
    if created_namespace {
        fs::create_dir(namespace).unwrap();
        fs::set_permissions(namespace, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(!paths.state_dir.exists());
    fs::create_dir(&paths.state_dir).unwrap();
    fs::set_permissions(&paths.state_dir, fs::Permissions::from_mode(0o700)).unwrap();
    struct OwnedState(std::path::PathBuf, Option<std::path::PathBuf>);
    impl Drop for OwnedState {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
            if let Some(namespace) = &self.1 {
                // Other tests may have adopted the shared namespace meanwhile.
                let _ = fs::remove_dir(namespace);
            }
        }
    }
    let _state = OwnedState(
        paths.state_dir.clone(),
        created_namespace.then(|| namespace.to_path_buf()),
    );
    let mut command = fixture.command();
    command
        .args(["serve", "--config"])
        .arg(fixture.root.join("aw.json"))
        .arg("--state-dir")
        .arg(&paths.state_dir)
        .env("XDG_RUNTIME_DIR", "");
    let service = Process::spawn(&fixture, command);
    let deadline = Instant::now() + TIMEOUT;
    let client = loop {
        if let Ok(client) =
            aw_service::Client::connect(&paths.socket, Instant::now() + Duration::from_millis(100))
        {
            break client;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    };
    let (_native, pid) = native(&fixture);
    success(install(&fixture, pid, false));
    let mut desired = document.clone();
    desired["spec"]["events"]["tool.before"] = json!({"enabled":true,"steps":[]});
    fixture.save(&desired);
    let mut update = fixture.command();
    update
        .env("XDG_RUNTIME_DIR", "")
        .args(["update", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder"]);
    success(fixture.run(update));
    success(install(&fixture, pid, true));
    let status = client
        .call(aw_service::Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(status["bindings"], 0);
    client
        .call(aw_service::Operation::Stop, Instant::now() + TIMEOUT)
        .unwrap();
    assert!(service.finish().status.success());
    assert!(!fixture
        .root
        .join("home/.qoder/.aw-attachment.json")
        .exists());
}

#[test]
fn empty_interrupted_claim_is_recoverable_without_stealing_a_live_lock() {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let fixture = Fixture::new();
    let service = Service::start(&fixture, &fixture.document());
    let (_process, pid) = native(&fixture);
    let ownership = fixture.root.join("home/.qoder/.aw-attachment.json");
    let create = || {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&ownership)
            .unwrap()
    };
    let lock = create();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(!install(&fixture, pid, false).status.success());
    assert!(!install(&fixture, pid, true).status.success());
    assert_eq!(fs::metadata(&ownership).unwrap().len(), 0);
    drop(lock);
    success(install(&fixture, pid, false));
    success(install(&fixture, pid, true));
    drop(create());
    success(install(&fixture, pid, true));
    assert!(!ownership.exists());
    assert_eq!(service.status()["bindings"], 0);
}

#[test]
fn killed_installer_during_bind_preparation_releases_binding_and_allows_retry() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    let (initial, _) = native(&fixture);
    drop(initial);
    fs::remove_file(fixture.root.join("native-pid.json")).unwrap();
    let mut command = native_command(&fixture);
    command
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root)
        .env("FAKE_PREPARE_BLOCK", "1");
    let _native = Process::spawn(&fixture, command);
    let pid = wait(&fixture.root.join("native-pid.json"))
        .as_u64()
        .unwrap() as u32;
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();
    let mut command = fixture.command();
    command
        .args(["agent", "connect", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--pid"])
        .arg(pid.to_string());
    let installer = Process::spawn(&fixture, command);
    let provider = wait(&fixture.root.join("preparing.pid")).as_u64().unwrap();
    installer.signal(libc::SIGKILL);
    assert!(!installer.finish().status.success());
    let deadline = Instant::now() + TIMEOUT;
    while service.status()["bindings"] != 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!std::path::Path::new(&format!("/proc/{provider}")).exists());
    assert_eq!(
        fs::metadata(fixture.root.join("home/.qoder/.aw-attachment.json"))
            .unwrap()
            .len(),
        0
    );
    fs::write(fixture.root.join("allow-prepare"), b"").unwrap();
    success(install(&fixture, pid, false));
    success(install(&fixture, pid, true));
    assert_eq!(service.status()["bindings"], 0);
    identity.check().unwrap();
    // As with aw run, SIGKILL cannot execute CLI file destructors. This owned
    // fixture removes abandoned staging with its private directory on Drop.
}
