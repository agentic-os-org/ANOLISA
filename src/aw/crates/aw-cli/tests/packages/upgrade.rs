//! New-prefix upgrades retain legacy registrations and rollback payloads in a shared database.

use super::{failure, make_private_dir, success, Fixture};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, u64, Vec<u8>)> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        result.insert(path.clone(), (metadata.mode(), metadata.ino(), bytes));
        if metadata.is_dir() {
            result.extend(snapshot(&path));
        }
    }
    result
}

fn legacy_registration(fixture: &Fixture, empty: bool) {
    success(fixture.install().output().unwrap());
    if empty {
        success(
            fixture
                .command()
                .args(["uninstall", "--yes", "--prefix"])
                .arg(&fixture.prefix)
                .output()
                .unwrap(),
        );
    }
    // Pre-#9222 used the same central records and these prefix-local locator names.
    let old = fixture.prefix.join(".aw-packages");
    fs::rename(fixture.prefix.join(".local-packages"), &old).unwrap();
    let marker = old.join("installation.json");
    let mut pointer: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    pointer.as_object_mut().unwrap().remove("package_manager");
    fs::write(
        old.join("aw-core.json"),
        serde_json::to_vec(&pointer).unwrap(),
    )
    .unwrap();
    fs::remove_file(marker).unwrap();
    fs::rename(
        fixture.prefix.join(".local-install.lock"),
        fixture.prefix.join(".aw-install.lock"),
    )
    .unwrap();
}

#[test]
fn new_prefix_upgrade_preserves_legacy_payloads_and_empty_registrations() {
    for empty in [false, true] {
        let fixture = Fixture::new();
        legacy_registration(&fixture, empty);
        let before = snapshot(&fixture.prefix);
        let database = fixture
            .root
            .join("package-state/aw/packages/installed.json");
        let original: Value = serde_json::from_slice(&fs::read(&database).unwrap()).unwrap();
        let fresh = fixture.root.join("new-prefix");
        success(
            fixture
                .command()
                .args(["install", "--bundle"])
                .arg(&fixture.bundle)
                .arg("--prefix")
                .arg(&fresh)
                .output()
                .unwrap(),
        );
        let committed: Value = serde_json::from_slice(&fs::read(&database).unwrap()).unwrap();
        assert_eq!(committed["installations"].as_array().unwrap().len(), 2);
        assert_eq!(committed["installations"][0], original["installations"][0]);
        assert_eq!(snapshot(&fixture.prefix), before);

        let listed: Value = serde_json::from_str(&success(
            fixture
                .command()
                .args(["list", "--installed", "--json"])
                .output()
                .unwrap(),
        ))
        .unwrap();
        assert_eq!(
            listed["result"]["inventory"]["installations"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        for args in [vec!["list", "--installed"], vec!["doctor"]] {
            success(
                fixture
                    .command()
                    .args(args)
                    .arg("--prefix")
                    .arg(&fresh)
                    .output()
                    .unwrap(),
            );
        }

        // Explicit operands keep rejecting legacy metadata without changing either install.
        let unchanged = fs::read(&database).unwrap();
        for args in [vec!["list", "--installed"], vec!["doctor"]] {
            assert!(failure(
                fixture
                    .command()
                    .args(args)
                    .arg("--prefix")
                    .arg(&fixture.prefix)
                    .output()
                    .unwrap(),
            )
            .contains("unsupported package metadata directory"));
        }
        assert!(failure(
            fixture
                .command()
                .args(["uninstall", "--yes", "--prefix"])
                .arg(&fixture.prefix)
                .output()
                .unwrap(),
        )
        .contains("previous-format records are unsupported"));
        assert!(failure(fixture.install().output().unwrap())
            .contains("previous-format records are unsupported"));
        assert!(failure(fixture.init().output().unwrap())
            .contains("unsupported package metadata directory"));
        assert!(!fixture.config.exists());
        assert!(!fixture.state.exists());
        assert_eq!(fs::read(&database).unwrap(), unchanged);
        assert_eq!(snapshot(&fixture.prefix), before);
        assert!(fresh.join("bin/provider").is_file());

        success(
            fixture
                .command()
                .args(["uninstall", "--yes", "--prefix"])
                .arg(&fresh)
                .output()
                .unwrap(),
        );
        assert!(!fresh.join("bin/provider").exists());
        let removed: Value = serde_json::from_slice(&fs::read(database).unwrap()).unwrap();
        assert_eq!(removed["installations"][0], original["installations"][0]);
        assert_eq!(snapshot(&fixture.prefix), before);
    }
}

#[test]
fn unregistered_legacy_receipts_do_not_block_a_fresh_database() {
    let fixture = Fixture::new();
    let records = fixture.root.join("old-prefix/.aw-packages");
    make_private_dir(&records);
    fs::write(records.join("aw-core.json"), b"unsupported legacy receipt").unwrap();
    let before = snapshot(records.parent().unwrap());
    success(fixture.install().output().unwrap());
    success(
        fixture
            .command()
            .args(["doctor", "--prefix"])
            .arg(&fixture.prefix)
            .output()
            .unwrap(),
    );
    assert_eq!(snapshot(records.parent().unwrap()), before);
}
