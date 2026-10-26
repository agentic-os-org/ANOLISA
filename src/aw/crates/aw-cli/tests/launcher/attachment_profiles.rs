//! Peer profile spellings must not bypass admission or mutate native registration.

#[path = "attachment_env.rs"]
mod dotenv;

use super::super::support::{Fixture, Process, Service};
use super::{app_command, install_entrypoint, success, wait, QWENPAW_PROCESSES};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn check_profile(profile_name: &str, peer_profile: Option<&str>, shared: bool) {
    let _apps = QWENPAW_PROCESSES.lock().unwrap();
    let fixture = Fixture::new();
    let home = fixture.root.join("home");
    let profile = home.join(profile_name);
    fs::create_dir(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("config.json"), b"{}").unwrap();
    // Both default directories exist in the legacy case: .copaw wins.
    if profile_name == ".copaw" {
        fs::create_dir(home.join(".qwenpaw")).unwrap();
    }
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/attach_app.py");
    fs::copy(&source, fixture.root.join("fake qwenpaw")).unwrap();
    fs::set_permissions(
        fixture.root.join("fake qwenpaw"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let port = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let selected_port = port();
    let mut document = fixture.document();
    document["spec"]["agents"]["qoder"] = json!({
        "adapter": "qwenpaw",
        "argv": [fixture.root.join("fake qwenpaw"), "app", "--host", "127.0.0.1",
                 "--port", selected_port.to_string()]
    });
    let service = Service::start(&fixture, &document);
    let mut command = app_command(&fixture);
    command
        .args(["app", "--host", "127.0.0.1", "--port"])
        .arg(selected_port.to_string())
        .current_dir(&fixture.root)
        .env("HOME", &home)
        .env("QWENPAW_WORKING_DIR", &profile)
        .env("FAKE_ROOT", &fixture.root)
        .env_remove("AW_NATIVE_CONFIG");
    let _native = Process::spawn(&fixture, command);
    let pid = wait(&fixture.root.join("native-pid.json"))
        .as_u64()
        .unwrap() as u32;
    let identity = aw_service::ProcessIdentity::read(pid).unwrap();

    let peer_root = fixture.root.join("peer");
    fs::create_dir(&peer_root).unwrap();
    fs::copy(source, peer_root.join("qwenpaw.py")).unwrap();
    let peer_home = if shared {
        home.clone()
    } else {
        peer_root.join("home")
    };
    fs::create_dir_all(&peer_home).unwrap();
    let mut command = std::process::Command::new(&fixture.python);
    command
        .args([
            "-u",
            "-m",
            "qwenpaw",
            "app",
            "--host",
            "127.0.0.1",
            "--port",
        ])
        .arg(port().to_string())
        .current_dir(&peer_root)
        .env("HOME", &peer_home)
        .env("FAKE_ROOT", &peer_root)
        .env("QWENPAW_RUNTIME_ID", "aw-profile-regression")
        .env_remove("AW_NATIVE_CONFIG")
        .env_remove("QWENPAW_WORKING_DIR");
    if let Some(value) = peer_profile {
        command.env("QWENPAW_WORKING_DIR", value);
    }
    let _peer = Process::spawn(&fixture, command);
    let peer_pid = wait(&peer_root.join("native-pid.json")).as_u64().unwrap() as u32;
    let output = install_entrypoint(&fixture, pid, false, &["agent", "connect"]);
    if shared {
        assert!(!output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("scope is shared"));
        assert_eq!(service.status()["bindings"], 0);
        assert!(!profile.join(".aw-attachment.json").exists());
        assert!(!profile.join("plugins/aw-native").exists());
        assert!(!fixture.root.join("post-seen").exists());
    } else {
        assert_eq!(success(output)["attached"], true);
        success(install_entrypoint(
            &fixture,
            pid,
            true,
            &["agent", "connect"],
        ));
    }
    assert!(!peer_root.join("post-seen").exists());
    assert!(!peer_home
        .join(".qwenpaw")
        .join("plugins/aw-native")
        .exists());
    identity.check().unwrap();
    aw_service::ProcessIdentity::read(peer_pid)
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn qwenpaw_profile_default_peer_is_shared() {
    check_profile(".qwenpaw", None, true);
}

#[test]
fn qwenpaw_profile_empty_peer_uses_default() {
    check_profile(".qwenpaw", Some(""), true);
}

#[test]
fn qwenpaw_profile_tilde_peer_is_shared() {
    check_profile(".qwenpaw", Some("~/.qwenpaw"), true);
}

#[test]
fn qwenpaw_profile_legacy_default_takes_precedence() {
    check_profile(".copaw", None, true);
    check_profile(".copaw", Some(""), true);
}

#[test]
fn qwenpaw_profile_relative_peer_uses_its_cwd() {
    check_profile(".qwenpaw", Some("../home/.qwenpaw"), true);
}

#[test]
fn qwenpaw_profile_different_home_remains_isolated() {
    check_profile(".qwenpaw", None, false);
    check_profile(".qwenpaw", Some("~/.qwenpaw"), false);
}
