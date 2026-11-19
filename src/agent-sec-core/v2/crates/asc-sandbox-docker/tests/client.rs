//! Local endpoint identity, daemon filesystem visibility and command timeouts.

use super::*;
use crate::backend_tests::fixture;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;

#[test]
fn activated_listener_requires_the_matching_open_descriptor() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = root.path().join("sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let proc = daemon_process(std::process::id().try_into().unwrap(), &endpoint).unwrap();
    assert!(owns_listener(&proc, &endpoint).unwrap());
    // Keep the kernel's real socket table but model a daemon without its fd.
    fs::create_dir(root.path().join("fd")).unwrap();
    std::os::unix::fs::symlink(proc.join("net"), root.path().join("net")).unwrap();
    assert!(!owns_listener(root.path(), &endpoint).unwrap());
    drop(listener);
    assert!(!owns_listener(&proc, &endpoint).unwrap());
}

#[test]
fn endpoint_requires_a_trusted_socket_and_ancestors() {
    let fixture = fixture();
    let root = tempfile::tempdir().unwrap();
    let endpoint = root.path().join("sock");
    let _listener = UnixListener::bind(&endpoint).unwrap();
    fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o777)).unwrap();
    let config = DockerConfig {
        host: format!("unix://{}", endpoint.display()),
        ..fixture.config()
    };
    assert!(
        matches!(Client::new(&config), Err(DockerError::Unsupported(s)) if s.contains("untrusted Docker control path"))
    );
    assert!(
        matches!(trusted_path(Path::new("/"), true), Err(DockerError::Unsupported(s)) if s.contains("not a Unix socket"))
    );
}

#[test]
fn mount_destinations_must_leave_all_client_control_paths_private() {
    let fixture = fixture();
    let config = fixture.config();
    let mut plan = crate::translate::translate(&fixture.spec(), &config, "test").unwrap();
    let mut client = Client::scripted(&config);
    client.guard = Some("/opt/guard/bin".into());
    client.verify_mounts(&plan).unwrap();
    for destination in ["/var/run", client.directory.to_str().unwrap(), "/opt/guard"] {
        plan.mounts[0].destination = destination.into();
        let error = client.verify_mounts(&plan).unwrap_err();
        assert!(matches!(error, DockerError::Unsupported(s) if s.contains("control paths")));
    }
}

#[test]
fn daemon_view_must_resolve_to_the_same_file() {
    let fixture = fixture();
    let mut client = Client::scripted(&fixture.config());
    let root = tempfile::tempdir().unwrap();
    let source = client.directory.join("source");
    fs::write(&source, b"same contents").unwrap();
    let other = root.path().join(source.strip_prefix("/").unwrap());
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::hard_link(&source, &other).unwrap();
    client.daemon_root = Some(root.path().into());
    client.verify_visible(&source).unwrap();
    fs::remove_file(&other).unwrap();
    fs::copy(&source, &other).unwrap();
    let error = client.verify_visible(&source).unwrap_err();
    assert!(
        matches!(error, DockerError::Unsupported(s) if s.contains("identical staging objects"))
    );
}

#[test]
fn timed_out_command_returns_unknown_state() {
    let fixture = fixture();
    let client = Client::scripted(&fixture.config());
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let error = client.output(command, Duration::ZERO).unwrap_err();
    assert!(matches!(error, DockerError::Protocol(s) if s.contains("state unknown")));
}
