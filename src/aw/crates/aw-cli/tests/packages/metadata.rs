//! Actual raw installs retain listable facts after losing required management metadata.

use super::{support::invoke, Fixture};
use serde_json::Value;
use std::{fs, path::Path};

fn query(fixture: &Fixture, doctor: bool, prefix: bool) -> Value {
    let mut command = fixture.command();
    if doctor {
        command.arg("doctor");
    } else {
        command.args(["list", "--installed"]);
    }
    command.arg("--json");
    if prefix {
        command.arg("--prefix").arg(&fixture.prefix);
    }
    let output = invoke(&command);
    assert_eq!(
        output.status.code(),
        Some(if doctor { 1 } else { 0 }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], if doctor { "failed" } else { "succeeded" });
    if doctor {
        assert_eq!(value["error"]["code"], "integrity_check");
    }
    value
}

fn missing(value: &Value, path: &Path) {
    assert!(
        value["result"]["report"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "missing" && issue["path"] == path.to_str().unwrap()),
        "{value}"
    );
}

fn install(fixture: &Fixture) {
    let output = invoke(&fixture.install());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn installed_raw_metadata_loss_fails_doctor_json_but_preserves_listed_package_facts() {
    for name in [".aw-install.lock", ".aw-packages/aw-core.json"] {
        let fixture = Fixture::new();
        install(&fixture);
        let database = fixture
            .root
            .join("package-state/aw/packages/installed.json");
        let committed = fs::read(&database).unwrap();
        let path = fixture.prefix.join(name);
        fs::remove_file(&path).unwrap();
        for prefix in [false, true] {
            let listed = query(&fixture, false, prefix);
            assert_eq!(
                listed["result"]["inventory"]["installations"][0]["packages"][0]["package_id"],
                "aw-core"
            );
            missing(&query(&fixture, true, prefix), &path);
        }
        assert_eq!(fs::read(database).unwrap(), committed);
        assert!(!path.exists());
        assert!(fixture.prefix.join("bin/aw").is_file());
    }
}

#[test]
fn removed_last_package_still_requires_the_registered_prefix_metadata() {
    let fixture = Fixture::new();
    install(&fixture);
    let output = invoke(
        fixture
            .command()
            .args(["uninstall", "--yes", "--prefix"])
            .arg(&fixture.prefix),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lock = fixture.prefix.join(".aw-install.lock");
    let marker = fixture.prefix.join(".aw-packages/aw-core.json");
    fs::remove_file(&lock).unwrap();
    fs::remove_file(&marker).unwrap();
    let listed = query(&fixture, false, true);
    assert!(
        listed["result"]["inventory"]["installations"][0]["packages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let checked = query(&fixture, true, true);
    assert_eq!(checked["result"]["report"]["files_checked"], 0);
    missing(&checked, &lock);
    missing(&checked, &marker);
}
