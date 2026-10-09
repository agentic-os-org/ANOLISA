//! Resolver and startup-file tests never start a daemon.

use super::*;
use serde_json::json;
use std::{
    os::unix::fs::{symlink, PermissionsExt},
    time::Duration,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        // Auto sockets append a revision to this directory. Keep that budget
        // independent of the checkout path, which is longer on cluster runners.
        let mut template = b"/tmp/aw-resolver-XXXXXX\0".to_vec();
        // SAFETY: mkdtemp receives a writable, terminated template and creates
        // an exclusively owned 0700 directory; Drop removes only that directory.
        let pointer = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        assert!(!pointer.is_null(), "{}", io::Error::last_os_error());
        let name = std::ffi::CStr::from_bytes_with_nul(&template).unwrap();
        Self(PathBuf::from(name.to_str().unwrap()))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn config() -> Value {
    json!({"apiVersion":"aw/v1alpha1","kind":"AWConfiguration","metadata":{"name":"launch"},"spec":{
        "daemon":{"startup":"on_demand","endpoint":"auto","state_dir":"auto"},
        "execution":{"guarantee":"native_hook","default_event_budget_ms":1000},
        "audit":{"enabled":true,"payload":"metadata_only"},
        "agents":{"qoder":{"adapter":"qoder","argv":["qodercli"]}},"providers":{},"events":{}}})
}
fn bytes(config: &Value) -> Vec<u8> {
    serde_json::to_vec(config).unwrap()
}

#[test]
fn auto_paths_are_private_deterministic_and_exact_byte_revision_bound() {
    let fixture = Fixture::new();
    let document = bytes(&config());
    let first = resolve(&document, Some(&fixture.0)).unwrap();
    assert_eq!(first, resolve(&document, Some(&fixture.0)).unwrap());
    assert!(first.state_dir.starts_with(fixture.0.join("aw")));
    assert_eq!(first.config_revision.len(), 64);
    let mut different = document.clone();
    different.push(b'\n');
    let second = resolve(&different, Some(&fixture.0)).unwrap();
    assert_ne!(first.state_dir, second.state_dir);
    assert_ne!(first.config_revision, second.config_revision);
    assert!(!fixture.0.join("aw").exists());
    let fallback = resolve(&document, None).unwrap();
    assert!(fallback
        .state_dir
        .starts_with(format!("/tmp/aw-{}", files::uid())));
}

#[test]
fn explicit_endpoint_infers_state_and_conflicting_paths_are_rejected() {
    let fixture = Fixture::new();
    let mut value = config();
    value["spec"]["daemon"]["endpoint"] = json!(fixture.0.join("state/aw.sock"));
    let paths = resolve(&bytes(&value), None).unwrap();
    assert_eq!(paths.state_dir, fixture.0.join("state"));
    value["spec"]["daemon"]["state_dir"] = json!(fixture.0.join("different"));
    assert!(resolve(&bytes(&value), None).is_err());
    value["spec"]["daemon"]["state_dir"] = json!("auto");
    value["spec"]["daemon"]["endpoint"] = json!(fixture.0.join("other.sock"));
    assert!(resolve(&bytes(&value), None).is_err());
    value["spec"]["daemon"]["endpoint"] = json!("relative/aw.sock");
    assert!(resolve(&bytes(&value), None).is_err());
}

#[test]
fn unsafe_runtime_paths_and_overlong_socket_paths_fail_without_fallback() {
    let fixture = Fixture::new();
    let document = bytes(&config());
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(resolve(&document, Some(&fixture.0)).is_err());
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
    let alias = fixture.0.join("alias");
    symlink(&fixture.0, &alias).unwrap();
    assert!(resolve(&document, Some(&alias)).is_err());
    assert!(resolve(&document, Some(Path::new("relative"))).is_err());
    let mut value = config();
    value["spec"]["daemon"]["state_dir"] = json!(format!("/{}", "x".repeat(108)));
    assert!(resolve(&bytes(&value), None).is_err());
}

#[test]
fn snapshots_keep_original_bytes_and_reject_existing_content_changes() {
    let fixture = Fixture::new();
    let mut value = config();
    value["spec"]["daemon"]["state_dir"] = json!(fixture.0);
    let mut document = bytes(&value);
    document.push(b'\n');
    let paths = resolve(&document, None).unwrap();
    let snapshot = files::snapshot(&paths, &document).unwrap();
    assert_eq!(fs::read(&snapshot).unwrap(), document);
    assert_eq!(fs::metadata(&snapshot).unwrap().mode() & 0o777, 0o400);
    assert_eq!(files::snapshot(&paths, &document).unwrap(), snapshot);
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&snapshot, b"different").unwrap();
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(files::snapshot(&paths, &document).is_err());
}

#[test]
fn startup_lock_obeys_deadline_and_rejects_symlink_replacement() {
    let fixture = Fixture::new();
    let held = files::lock(&fixture.0, Instant::now() + Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    assert!(files::lock(&fixture.0, started + Duration::from_millis(30)).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(held);
    fs::remove_file(fixture.0.join("launch.lock")).unwrap();
    let target = fixture.0.join("target");
    fs::write(&target, b"do not change").unwrap();
    symlink(&target, fixture.0.join("launch.lock")).unwrap();
    assert!(files::lock(&fixture.0, Instant::now() + Duration::from_secs(1)).is_err());
    assert_eq!(fs::read(target).unwrap(), b"do not change");
}

#[test]
fn external_absence_has_no_filesystem_or_process_side_effects() {
    let fixture = Fixture::new();
    let mut value = config();
    value["spec"]["daemon"]["startup"] = json!("external");
    value["spec"]["daemon"]["state_dir"] = json!(fixture.0.join("missing"));
    assert!(ensure_service(
        &bytes(&value),
        Path::new("/must-not-be-executed"),
        Instant::now() + Duration::from_secs(1)
    )
    .is_err());
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn old_protocol_daemon_is_rejected_before_reuse_or_replacement() {
    use std::{os::unix::net::UnixListener, thread};

    for startup in ["on_demand", "external"] {
        let fixture = Fixture::new();
        let mut value = config();
        value["spec"]["daemon"]["state_dir"] = json!(fixture.0.join("state"));
        value["spec"]["daemon"]["startup"] = json!(startup);
        let document = bytes(&value);
        let paths = resolve(&document, None).unwrap();
        files::private_directory(&paths.state_dir).unwrap();
        let listener = UnixListener::bind(&paths.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600)).unwrap();
        let inode = fs::metadata(&paths.socket).unwrap().ino();
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = thread::scope(|scope| {
            let server = scope.spawn(|| {
                // The old daemon accepts status discovery but cannot parse the
                // new native_environment field in later callback requests.
                for _ in 0..2 {
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                assert!(Instant::now() < deadline);
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("fixture accept failed: {error}"),
                        }
                    };
                    let request: Value = crate::ipc::read(&mut stream, deadline).unwrap();
                    assert_eq!(request["operation"]["method"], "status");
                    crate::ipc::write(&mut stream, &json!({
                        "api_version":"aw-service/v1alpha1",
                        "identity":{"generation":"old-daemon", "config_revision":paths.config_revision},
                        "result":{"pid":std::process::id(), "audit_healthy":true},
                        "error":null, "audit_key":null,
                    }), deadline).unwrap();
                    if request["api_version"] != "aw-service/v1alpha1" {
                        break;
                    }
                }
            });
            let result = ensure_service(&document, Path::new("/must-not-be-executed"), deadline);
            server.join().unwrap();
            result
        });
        assert!(
            matches!(result, Err(crate::Error::Rejected(ref code)) if code == "protocol_version")
        );
        assert_eq!(fs::metadata(&paths.socket).unwrap().ino(), inode);
        assert!(!paths.state_dir.join("service.log").exists());
    }
}
