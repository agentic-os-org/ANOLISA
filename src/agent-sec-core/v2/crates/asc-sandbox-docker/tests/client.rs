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

#[test]
fn untrusted_cli_ancestors_are_rejected_even_for_a_trusted_symlink_target() {
    let fixture = fixture();
    let root = tempfile::tempdir().unwrap();
    let alias = root.path().join("client");
    std::os::unix::fs::symlink("/usr/bin/true", &alias).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
    let error = crate::DockerBackend::new(DockerConfig {
        cli: alias,
        ..fixture.config()
    })
    .unwrap_err();
    assert!(matches!(error, DockerError::Unsupported(s) if s.contains("untrusted Docker CLI")));
}

#[test]
fn writable_grants_cannot_expose_directories_in_the_cli_symlink_chain() {
    let fixture = fixture();
    let mut config = fixture.config();
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("tools");
    let links = root.path().join("links");
    fs::create_dir(&tools).unwrap();
    fs::create_dir(&links).unwrap();
    std::os::unix::fs::symlink("../links", tools.join("current")).unwrap();
    std::os::unix::fs::symlink(&config.cli, links.join("docker")).unwrap();
    config.cli = tools.join("current/docker");

    let client = Client::scripted(&config);
    let mut plan = crate::translate::translate(&fixture.spec(), &config, "test").unwrap();
    plan.mounts.retain(|mount| !mount.read_only);
    client.verify_mounts(&plan).unwrap();
    let allowed = plan.mounts[0].clone();
    for directory in [&tools, &links] {
        let name = directory.to_str().unwrap();
        for (source, destination) in [
            (name, allowed.destination.as_str()),
            (allowed.source.as_str(), name),
        ] {
            plan.mounts[0].source = source.into();
            plan.mounts[0].destination = destination.into();
            assert!(
                matches!(client.verify_mounts(&plan), Err(DockerError::Unsupported(s)) if s == "writable grant exposes the Docker CLI")
            );
        }
    }
}

#[test]
fn guard_commands_use_the_open_executable_after_path_replacement() {
    let fixture = fixture();
    let config = fixture.config();
    let client = Client::scripted(&config);
    let replacement = config.cli.with_extension("replacement");
    fs::write(&replacement, b"inert replacement").unwrap();
    fs::rename(&replacement, &config.cli).unwrap();
    for args in [&["version"][..], &["rm", "-f", "test"][..]] {
        let output = client.command(args).output().unwrap();
        assert!(output.status.success());
    }
    assert_eq!(fixture.read_calls("rm.calls"), ["-f", "test"]);
}

#[test]
fn writable_grants_cannot_expose_the_cli_by_name_or_object_identity() {
    let fixture = fixture();
    let config = fixture.config();
    let client = Client::scripted(&config);
    let mut plan = crate::translate::translate(&fixture.spec(), &config, "test").unwrap();
    plan.mounts.retain(|mount| !mount.read_only);
    let mount = &mut plan.mounts[0];
    mount.destination = config.cli.parent().unwrap().to_str().unwrap().into();
    assert!(
        matches!(client.verify_mounts(&plan), Err(DockerError::Unsupported(s)) if s.contains("Docker CLI"))
    );
    plan.mounts[0].destination = "/work".into();
    for source in [config.cli.as_path(), config.cli.parent().unwrap()] {
        plan.mounts[0].source = source.to_str().unwrap().into();
        assert!(
            matches!(client.verify_mounts(&plan), Err(DockerError::Unsupported(s)) if s.contains("Docker CLI"))
        );
    }
    let alias = config.cli.with_extension("link");
    fs::hard_link(&config.cli, alias).unwrap();
    plan.mounts[0].source = fixture.spec().launch.cwd;
    assert!(
        matches!(client.verify_mounts(&plan), Err(DockerError::Unsupported(s)) if s.contains("singly linked"))
    );
}
