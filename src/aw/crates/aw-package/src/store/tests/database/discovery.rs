//! Incidental discovery preserves current locator validation and explicit legacy rejection.

use super::{snapshot, Fixture};
use crate::store::{database::Inventory, plan};
use serde_json::{json, Value};
use std::fs;

#[test]
fn legacy_registration_is_listable_but_explicit_operands_remain_unsupported() {
    let fixture = Fixture::new();
    fixture.database(vec![fixture.installation()]);
    fixture.record(
        &fixture.prefix.join(".aw-packages/aw-core.json"),
        b"not decoded during discovery",
        0o600,
    );
    let before = snapshot(fixture.directory.path());
    assert_eq!(fixture.read().unwrap().installations.len(), 1);
    for result in [
        Inventory::read(&fixture.paths, Some(&fixture.prefix)).map(|_| ()),
        plan::uninstall(&fixture.paths, &fixture.prefix, &[]).map(|_| ()),
    ] {
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unsupported package metadata directory"));
    }
    let report = fixture.read().unwrap().doctor().unwrap();
    assert!(!report.healthy());
    assert!(report.issues.iter().any(|issue| issue
        .message
        .contains("unsupported package metadata directory")));
    assert_eq!(snapshot(fixture.directory.path()), before);
}

#[test]
fn legacy_metadata_cannot_hide_conflicting_or_unsafe_current_locators() {
    for case in [
        "id",
        "root",
        "manager",
        "scope",
        "malformed",
        "path",
        "extra",
    ] {
        let fixture = Fixture::new();
        fixture.database(vec![fixture.installation()]);
        fixture.record(
            &fixture.prefix.join(".aw-packages/aw-core.json"),
            b"legacy",
            0o600,
        );
        fixture.registration(&fixture.prefix, "install-1");
        let marker = fixture.prefix.join(".local-packages/installation.json");
        let mut pointer: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        match case {
            "id" => pointer["installation_id"] = json!("conflicting-id"),
            "root" => pointer["database_root"] = json!(fixture.directory.path().join("other")),
            "manager" => pointer["package_manager"] = json!("anolisa"),
            "scope" => pointer["scope"] = json!("unknown"),
            "malformed" => pointer = json!({"not": "a locator"}),
            "path" => pointer["database_root"] = json!(fixture.directory.path().join("../other")),
            "extra" => fixture.record(&marker.with_file_name("extra.json"), b"{}", 0o600),
            _ => unreachable!(),
        }
        fs::write(&marker, serde_json::to_vec(&pointer).unwrap()).unwrap();
        let before = snapshot(fixture.directory.path());
        assert!(fixture.read().is_err(), "accepted {case}");
        assert_eq!(snapshot(fixture.directory.path()), before, "{case}");
    }
}
