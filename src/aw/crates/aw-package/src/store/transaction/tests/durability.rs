//! A visible database rename must not retire recovery bytes before directory sync succeeds.

use super::*;
use crate::{NoticeCode, RecoveryState};

fn pending(fixture: &Fixture) -> journal::Journal {
    let mut entries = fs::entries(&fixture.paths.root.join("transactions")).unwrap();
    assert_eq!(entries.len(), 1);
    journal::read(&fixture.paths, &entries.remove(0)).unwrap()
}

#[test]
fn failed_database_directory_sync_retains_backups_until_recovery_confirms_commit() {
    let fixture = Fixture::new();
    let changes = fixture.changes(false);
    super::super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).unwrap();
    let removal = crate::plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal.ready(), "{:?}", removal.problems);

    let fault = fs::fail_database_sync(&fixture.paths.installed());
    let failure =
        crate::plan::execute(&fixture.paths, &removal, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(&failure.error, Error::Io(error) if error.raw_os_error() == Some(libc::EIO)));
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Incomplete);
    let entry = pending(&fixture);
    assert_eq!(
        disk::read(fixture.paths.installed()).unwrap(),
        entry.after_database
    );
    assert!(failure.report.notices.iter().any(
        |notice| notice.code == NoticeCode::BackupRetained && notice.path == entry.directory()
    ));
    let action = entry
        .actions
        .iter()
        .find(|action| action.path == "bin/tool")
        .unwrap();
    let backup = entry.workspace().join(&action.backup);
    assert_eq!(disk::read(&backup).unwrap(), b"new tool\n");
    assert!(!fixture.prefix.join("bin/tool").exists());
    assert!(matches!(
        Inventory::read(&fixture.paths, None),
        Err(Error::NeedsRecovery(_))
    ));

    for _ in 0..2 {
        let error = super::super::recover(&fixture.paths, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(error, Error::Io(error) if error.raw_os_error() == Some(libc::EIO)));
        assert!(entry.path().is_file());
        assert_eq!(disk::read(&backup).unwrap(), b"new tool\n");
    }
    drop(fault);
    let recovered = fixture.recover();
    assert_eq!((recovered.committed, recovered.rolled_back), (1, 0));
    assert!(recovered.conflicts.is_empty());
    assert!(!entry.directory().exists());
    assert!(!backup.exists());
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert!(inventory.installations[0].packages.is_empty());
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    assert_eq!(fixture.recover().committed, 0);
}

#[test]
fn failed_commit_sync_keeps_rollback_material_if_the_old_database_returns() {
    let fixture = Fixture::new();
    let changes = fixture.changes(true);
    let original_record = changes.marker_before.clone().unwrap();
    let fault = fs::fail_database_sync(&fixture.paths.installed());
    let failure =
        super::super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).unwrap_err();
    assert!(!failure.report.committed);
    assert_eq!(failure.report.recovery, RecoveryState::Incomplete);
    let entry = pending(&fixture);
    assert_eq!(
        disk::read(fixture.paths.installed()).unwrap(),
        entry.after_database
    );
    assert!(fixture.prefix.join("bin/tool").is_file());
    assert!(serde_json::from_slice::<RegistrationPointer>(
        &disk::read(fixture.prefix.join(".aw-packages/aw-core.json")).unwrap()
    )
    .is_ok());
    drop(fault);

    // Model the namespace reverting after power loss before the failed directory sync.
    fixture.record(
        &fixture.paths.installed(),
        entry.before_database.as_ref().unwrap(),
        0o644,
    );
    let recovered = fixture.recover();
    assert_eq!((recovered.committed, recovered.rolled_back), (0, 1));
    assert!(recovered.conflicts.is_empty());
    assert!(!fixture.prefix.join("bin/tool").exists());
    assert_eq!(
        disk::read(fixture.prefix.join("bin/aw")).unwrap(),
        b"old core\n"
    );
    assert_eq!(
        disk::read(fixture.prefix.join(".aw-packages/aw-core.json")).unwrap(),
        original_record
    );
    assert!(!entry.directory().exists());
    assert!(Inventory::read(&fixture.paths, None)
        .unwrap()
        .installations
        .is_empty());
    assert_eq!(fixture.recover().rolled_back, 0);
}
