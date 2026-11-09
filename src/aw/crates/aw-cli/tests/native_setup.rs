//! Explicit startup setup writes private assets without changing native settings.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt};
use support::Fixture;

#[test]
fn python_setup_is_explicit_private_and_preserves_existing_startup() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"native":{"adapter":"qwenpaw","argv":["qwenpaw","app"]}});
    fixture.save(&document);
    let environment = fixture.root.join("venv");
    let site = environment.join("lib/python3.12/site-packages");
    fs::create_dir_all(&site).unwrap();
    fs::set_permissions(&site, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(environment.join("pyvenv.cfg"), "home = /fixture/python\n").unwrap();
    fs::write(site.join("sitecustomize.py"), "USER_STARTUP = True\n").unwrap();
    let setup = || {
        let mut command = fixture.command();
        command
            .args(["instance", "setup", "--config"])
            .arg(fixture.root.join("aw.json"))
            .args(["--agent", "native", "--directory"])
            .arg(&site);
        command
    };
    let receipt = fixture.successful(setup());
    assert_eq!(receipt["environment"].as_object().unwrap().len(), 1);
    assert_eq!(receipt["files"].as_array().unwrap().len(), 3);
    for path in receipt["files"].as_array().unwrap() {
        assert_eq!(
            fs::metadata(path.as_str().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let descriptor: Value = serde_json::from_slice(
        &fs::read(
            receipt["environment"]["AW_INSTANCE_BOOTSTRAP"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(descriptor.as_object().unwrap().len(), 3);
    assert_eq!(descriptor["agent"], "native");
    assert!(!fixture.run(setup()).status.success());
    assert_eq!(
        fs::read_to_string(site.join("sitecustomize.py")).unwrap(),
        "USER_STARTUP = True\n"
    );
    assert!(!fixture.root.join("state").exists());
}
