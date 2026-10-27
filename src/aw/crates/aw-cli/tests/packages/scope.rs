//! Exercise scope selection without writing the real system database or requiring root.

use super::support::{invoke, Fixture};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

fn report(command: &mut Command, code: i32) -> Value {
    let output = invoke(command);
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn extension(fixture: &Fixture) -> PathBuf {
    let bundle = fixture.root.join("another-provider-bundle");
    let path = bundle.join("payload/bin/other-provider");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"offline Provider fixture").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let description = json!({
        "schema_version": 1,
        "package_id": "another.provider",
        "version": "2.0.0",
        "os": "linux",
        "arch": std::env::consts::ARCH,
        "files": {"bin/other-provider": {
            "sha256": aw_package::digest(&path).unwrap(), "mode": 0o755
        }},
        "dependencies": [{"package_id": "aw-core", "version": "1.0.0"}],
        "provider": {"protocol": "aw-provider/v1alpha1"}
    });
    fs::write(
        bundle.join("package.json"),
        serde_json::to_vec(&description).unwrap(),
    )
    .unwrap();
    bundle
}

#[test]
fn explicit_system_dry_run_selects_system_scope_without_creating_state() {
    let fixture = Fixture::new();
    // This reads system scope and tests the real selector; it never performs a root install.
    let system = report(
        fixture
            .install()
            .args(["--scope", "system", "--dry-run", "--json"]),
        0,
    );
    assert_eq!(system["result"]["plan"]["scope"], "system");
    assert!(!fixture.prefix.exists());
    assert!(!fixture.root.join("package-state").exists());
    let default = report(fixture.install().args(["--dry-run", "--json"]), 0);
    assert_eq!(default["result"]["plan"]["scope"], "user");
    assert!(!fixture.prefix.exists());
    assert!(!fixture.root.join("package-state").exists());
}

#[test]
fn split_extensions_and_uninstall_require_consistent_explicit_scope() {
    let fixture = Fixture::new();
    let provider = extension(&fixture);
    report(fixture.install().args(["--scope", "user", "--json"]), 0);
    let append = |scope: &str| {
        let mut command = fixture.command();
        command
            .args(["install", "--scope", scope, "--json", "--bundle"])
            .arg(&provider)
            .arg("--prefix")
            .arg(&fixture.prefix);
        command
    };
    let before = fs::read(fixture.prefix.join(".local-packages/installation.json")).unwrap();
    let mut wrong = append("system");
    wrong.arg("--dry-run");
    let rejected = report(&mut wrong, 1);
    assert_eq!(rejected["error"]["code"], "package_contract");
    assert_eq!(
        fs::read(fixture.prefix.join(".local-packages/installation.json")).unwrap(),
        before
    );
    assert!(!fixture.prefix.join("bin/other-provider").exists());
    report(&mut append("user"), 0);
    assert!(fixture.prefix.join("bin/aw").is_file());
    assert!(fixture.prefix.join("bin/other-provider").is_file());
    let removal = |scope: &str| {
        let mut command = fixture.command();
        command
            .args(["uninstall", "--scope", scope, "--yes", "--json", "--prefix"])
            .arg(&fixture.prefix);
        command
    };
    let mut wrong = removal("system");
    wrong.arg("--dry-run");
    assert_eq!(report(&mut wrong, 1)["error"]["code"], "package_contract");
    assert!(fixture.prefix.join("bin/aw").is_file());
    assert!(fixture.prefix.join("bin/other-provider").is_file());
    report(&mut removal("user"), 0);
    assert!(!fixture.prefix.join("bin/aw").exists());
    assert!(!fixture.prefix.join("bin/other-provider").exists());
}
