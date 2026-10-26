//! Central listing tolerates lost metadata; raw-file verification must report that damage.

use super::*;

fn metadata(fixture: &Fixture) {
    let records = fixture.prefix.join(".aw-packages");
    fs::create_dir_all(&records).unwrap();
    fs::set_permissions(&records, fs::Permissions::from_mode(0o755)).unwrap();
    let marker = records.join("aw-core.json");
    let pointer = RegistrationPointer::new(&fixture.paths, "install-1".into()).unwrap();
    fs::write(&marker, serde_json::to_vec(&pointer).unwrap()).unwrap();
    fs::set_permissions(marker, fs::Permissions::from_mode(0o644)).unwrap();
    let lock = fixture.prefix.join(".aw-install.lock");
    fs::write(&lock, []).unwrap();
    fs::set_permissions(lock, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn registered_raw_metadata_loss_is_reported_while_central_listing_remains_available() {
    for name in [".aw-install.lock", ".aw-packages/aw-core.json"] {
        let fixture = Fixture::new();
        fixture.database(vec![fixture.installation()]);
        metadata(&fixture);
        assert!(fixture.read().unwrap().doctor().unwrap().healthy());
        let missing = fixture.prefix.join(name);
        fs::remove_file(&missing).unwrap();
        let before = snapshot(fixture.directory.path());
        let inventory = fixture.read().unwrap();
        assert_eq!(inventory.installations[0].packages.len(), 1);
        let report = inventory.doctor().unwrap();
        assert!(!report.healthy());
        assert_eq!(report.files_checked, 1);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == DoctorIssueCode::Missing && issue.path == missing));
        assert_eq!(snapshot(fixture.directory.path()), before);
    }
}

#[test]
fn empty_registered_raw_installations_still_require_their_locator_and_lock() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation();
    installation["packages"] = json!([]);
    fixture.database(vec![installation]);
    metadata(&fixture);
    assert!(fixture.read().unwrap().doctor().unwrap().healthy());
    let lock = fixture.prefix.join(".aw-install.lock");
    let marker = fixture.prefix.join(".aw-packages/aw-core.json");
    fs::remove_file(&lock).unwrap();
    fs::remove_file(&marker).unwrap();
    let inventory = fixture.read().unwrap();
    assert!(inventory.installations[0].packages.is_empty());
    let report = inventory.doctor().unwrap();
    assert_eq!(report.packages_checked, 0);
    assert_eq!(report.files_checked, 0);
    assert_eq!(report.issues.len(), 2);
    for path in [lock, marker] {
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == DoctorIssueCode::Missing && issue.path == path));
    }
}

#[test]
fn native_records_do_not_require_raw_management_metadata_and_legacy_records_keep_their_format() {
    let fixture = Fixture::new();
    let mut installation = fixture.installation();
    installation["manager"] = json!("rpm");
    fixture.database(vec![installation]);
    let report = fixture.read().unwrap().doctor().unwrap();
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].code, DoctorIssueCode::NativeManager);
    assert_eq!(report.files_checked, 0);
    assert!(!fixture.prefix.join(".aw-install.lock").exists());
    assert!(!fixture.prefix.join(".aw-packages").exists());

    let legacy = Fixture::new();
    legacy.legacy("1.2.3");
    let before = snapshot(legacy.directory.path());
    let inventory = Inventory::read(&legacy.paths, Some(&legacy.prefix)).unwrap();
    assert_eq!(inventory.source, InventorySource::Legacy);
    assert!(inventory.doctor().unwrap().healthy());
    assert_eq!(snapshot(legacy.directory.path()), before);
}

#[test]
fn a_symlinked_registered_lock_is_reported_without_following_or_replacing_it() {
    let fixture = Fixture::new();
    fixture.database(vec![fixture.installation()]);
    metadata(&fixture);
    let lock = fixture.prefix.join(".aw-install.lock");
    let target = fixture.directory.path().join("unrelated.txt");
    fs::write(&target, b"unrelated private data").unwrap();
    fs::remove_file(&lock).unwrap();
    symlink(&target, &lock).unwrap();
    let inventory = fixture.read().unwrap();
    let report = inventory.doctor().unwrap();
    assert!(!report.healthy());
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == DoctorIssueCode::Symlink && issue.path == lock));
    assert!(lock.is_symlink());
    assert_eq!(fs::read(target).unwrap(), b"unrelated private data");
}
