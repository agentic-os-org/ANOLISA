//! Initial procfs environment cannot prove profiles selected later by native dotenv.

use super::super::super::support::{Fixture, Process, Service};
use super::super::{app_command, install_entrypoint, success, wait, QWENPAW_PROCESSES};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[derive(Clone, Copy)]
enum Scenario {
    Shared,
    Removed,
    NoFile,
    Explicit,
    Disabled,
    HomeAlias,
    Empty,
}

fn check(scenario: Scenario) {
    let _apps = QWENPAW_PROCESSES.lock().unwrap();
    let fixture = Fixture::new();
    let home = fixture.root.join("home");
    let profile = home.join(".qwenpaw");
    fs::create_dir(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("config.json"), b"{}").unwrap();
    let sources = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher");
    fs::copy(
        sources.join("attach_app.py"),
        fixture.root.join("fake qwenpaw"),
    )
    .unwrap();
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
    document["spec"]["agents"]["qoder"] = json!({"adapter":"qwenpaw",
        "argv":[fixture.root.join("fake qwenpaw"),"app","--host","127.0.0.1",
        "--port",selected_port.to_string()]});
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
    let peer_home = peer_root.join("home");
    let independent = peer_home.join(".qwenpaw");
    fs::create_dir_all(&independent).unwrap();
    fs::copy(sources.join("attach_env.py"), peer_root.join("qwenpaw.py")).unwrap();
    fs::create_dir_all(peer_root.join("lib/qwenpaw")).unwrap();
    let constant = std::env::var_os("AW_TEST_QWENPAW_CONSTANT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| sources.join("profile_constant.py"));
    fs::copy(constant, peer_root.join("lib/qwenpaw/constant.py")).unwrap();
    let env_file = peer_root.join(".env");
    if !matches!(scenario, Scenario::NoFile) {
        fs::write(
            &env_file,
            format!(
                "QWENPAW_WORKING_DIR={}\nHOME={}\n",
                profile.display(),
                home.display()
            ),
        )
        .unwrap();
    }
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
        .env("FAKE_APP_SOURCE", fixture.root.join("fake qwenpaw"))
        .env_remove("QWENPAW_WORKING_DIR")
        .env_remove("QWENPAW_RUNTIME_ID")
        .env_remove("FAKE_REMOVE_DOTENV")
        .env_remove("AW_NATIVE_CONFIG");
    match scenario {
        Scenario::Explicit => {
            command.env("QWENPAW_WORKING_DIR", &independent);
        }
        Scenario::Empty => {
            command.env("QWENPAW_WORKING_DIR", "");
        }
        Scenario::Disabled => {
            command.env("QWENPAW_RUNTIME_ID", "aw-profile-test");
        }
        Scenario::HomeAlias => {
            command
                .env("QWENPAW_WORKING_DIR", "~/.qwenpaw")
                .env_remove("HOME");
        }
        Scenario::Removed => {
            command.env("FAKE_REMOVE_DOTENV", "1");
        }
        Scenario::Shared | Scenario::NoFile => {}
    }
    let _peer = Process::spawn(&fixture, command);
    let peer_pid = wait(&peer_root.join("native-pid.json")).as_u64().unwrap() as u32;
    let effective: Value = wait(&peer_root.join("effective-profile.json"));
    let shared = matches!(
        scenario,
        Scenario::Shared | Scenario::Removed | Scenario::HomeAlias
    );
    assert_eq!(
        effective,
        json!(if shared { &profile } else { &independent })
    );
    let initial = fs::read(format!("/proc/{peer_pid}/environ")).unwrap();
    let initial_profile = initial
        .split(|byte| *byte == 0)
        .any(|pair| pair.starts_with(b"QWENPAW_WORKING_DIR="));
    assert_eq!(
        initial_profile,
        matches!(
            scenario,
            Scenario::Explicit | Scenario::Empty | Scenario::HomeAlias
        )
    );
    if matches!(scenario, Scenario::Removed | Scenario::NoFile) {
        assert!(!env_file.exists());
    }
    let output = install_entrypoint(&fixture, pid, false, &["agent", "connect"]);
    let unconfirmed = shared || matches!(scenario, Scenario::NoFile);
    if unconfirmed {
        assert!(!output.status.success(), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("peer profile is unconfirmed"), "{error}");
        assert!(error.contains(&peer_pid.to_string()), "{error}");
        assert_eq!(service.status()["bindings"], 0);
        assert!(!fixture.root.join("post-seen").exists());
    } else {
        assert_eq!(success(output)["attached"], true);
        success(install_entrypoint(
            &fixture,
            pid,
            true,
            &["agent", "connect"],
        ));
        assert_eq!(service.status()["bindings"], 0);
    }
    assert!(!profile.join(".aw-attachment.json").exists());
    assert!(!profile.join("plugins/aw-native").exists());
    assert!(!independent.join("plugins/aw-native").exists());
    assert_eq!(fs::read(profile.join("config.json")).unwrap(), b"{}");
    assert!(!peer_root.join("post-seen").exists());
    identity.check().unwrap();
    aw_service::ProcessIdentity::read(peer_pid)
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn qwenpaw_dotenv_shared_peer_is_rejected_before_writes() {
    check(Scenario::Shared);
}

#[test]
fn qwenpaw_dotenv_removed_after_startup_does_not_prove_isolation() {
    check(Scenario::Removed);
}

#[test]
fn qwenpaw_dotenv_absence_does_not_prove_an_initially_missing_profile() {
    check(Scenario::NoFile);
}

#[test]
fn qwenpaw_dotenv_preserves_explicit_independent_profile() {
    check(Scenario::Explicit);
}

#[test]
fn qwenpaw_dotenv_runtime_id_keeps_independent_default_profile() {
    check(Scenario::Disabled);
}

#[test]
fn qwenpaw_dotenv_home_alias_requires_initial_home() {
    check(Scenario::HomeAlias);
}

#[test]
fn qwenpaw_dotenv_preserves_initially_empty_profile() {
    check(Scenario::Empty);
}
