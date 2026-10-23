//! Exercise scope selection without writing the real system database or requiring root.

use super::support::{invoke, Fixture};
use aw_package::{Component, Manifest, Payload};
use serde_json::Value;
use std::{
    collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command,
};

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

fn tokenless(fixture: &Fixture) -> PathBuf {
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(fixture.bundle.join("manifest.json")).unwrap()).unwrap();
    let core = &manifest.components[0];
    let bundle = fixture.root.join("tokenless-bundle");
    let mut files = BTreeMap::new();
    for program in ["aw-provider-tokenless", "tokenless", "rtk"] {
        let name = format!("libexec/aw/providers/tokenless/{program}");
        let path = bundle.join("payload").join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("offline package fixture: {program}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        files.insert(
            name,
            Payload {
                sha256: aw_package::digest(&path).unwrap(),
                mode: 0o755,
            },
        );
    }
    let provider = Component {
        format: core.format,
        component: "aw-provider-tokenless".into(),
        version: core.version.clone(),
        source_commit: core.source_commit.clone(),
        os: core.os.clone(),
        arch: core.arch.clone(),
        provider_protocol: core.provider_protocol.clone(),
        requires: BTreeMap::from([("aw-core".into(), core.version.clone())]),
        files,
    };
    fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec(&Manifest {
            format: 1,
            components: vec![provider],
        })
        .unwrap(),
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
fn split_core_extension_and_uninstall_require_consistent_explicit_scope() {
    let fixture = Fixture::new();
    let provider = tokenless(&fixture);
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
    let before = fs::read(fixture.prefix.join(".aw-packages/aw-core.json")).unwrap();
    let mut wrong = append("system");
    wrong.arg("--dry-run");
    let rejected = report(&mut wrong, 1);
    assert_eq!(rejected["error"]["code"], "package_contract");
    assert_eq!(
        fs::read(fixture.prefix.join(".aw-packages/aw-core.json")).unwrap(),
        before
    );
    assert!(!fixture
        .prefix
        .join("libexec/aw/providers/tokenless/rtk")
        .exists());
    report(&mut append("user"), 0);
    assert!(fixture.prefix.join("bin/aw").is_file());
    assert!(fixture
        .prefix
        .join("libexec/aw/providers/tokenless/rtk")
        .is_file());
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
    assert!(fixture
        .prefix
        .join("libexec/aw/providers/tokenless/rtk")
        .is_file());
    report(&mut removal("user"), 0);
    assert!(!fixture.prefix.join("bin/aw").exists());
    assert!(!fixture
        .prefix
        .join("libexec/aw/providers/tokenless/rtk")
        .exists());
}
