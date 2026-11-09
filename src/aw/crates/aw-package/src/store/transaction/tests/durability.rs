//! A visible database rename must not retire recovery bytes before directory sync succeeds.

use super::*;
use crate::store::{MutationFailure, NoticeCode, RecoveryState};

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
    let removal = crate::store::plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal.ready(), "{:?}", removal.problems);

    let fault = fs::fail_database_sync(&fixture.paths.installed());
    let failure =
        crate::store::plan::execute(&fixture.paths, &removal, &AtomicBool::new(false)).unwrap_err();
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
        &disk::read(fixture.prefix.join(".local-packages/installation.json")).unwrap()
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
        disk::read(fixture.prefix.join("bin/existing-tool")).unwrap(),
        b"existing tool\n"
    );
    assert_eq!(
        disk::read(fixture.prefix.join(".local-packages/installation.json")).unwrap(),
        original_record
    );
    assert!(!entry.directory().exists());
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert_eq!(inventory.installations.len(), 1);
    assert_eq!(inventory.installations[0].packages.len(), 1);
    assert_eq!(
        inventory.installations[0].packages[0].package_id,
        "example.existing"
    );
    drop(inventory);
    assert_eq!(fixture.recover().rolled_back, 0);
}

fn is_staging(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".aw-staging-"))
}

fn staging_failure(fixture: &Fixture, write: bool) -> MutationFailure {
    let changes = fixture.changes(false);
    let result = if write {
        let _fault = fs::fail_staging_write(&fixture.prefix);
        super::super::apply(&fixture.paths, &changes, &AtomicBool::new(false))
    } else {
        let _fault = fs::fail_staging_sync(&fixture.prefix);
        super::super::apply(&fixture.paths, &changes, &AtomicBool::new(false))
    };
    result.unwrap_err()
}

#[test]
fn staging_only_transaction_directory_is_retired_without_a_journal() {
    let fixture = Fixture::new();
    super::super::apply(
        &fixture.paths,
        &fixture.changes(false),
        &AtomicBool::new(false),
    )
    .unwrap();
    let transactions = fixture.paths.root.join("transactions");
    assert!(fs::entries(&transactions).unwrap().is_empty());

    let transaction = journal::new_id().unwrap();
    let directory = transactions.join(&transaction);
    fixture.directory(&directory);
    for index in 0..2 {
        let staging = directory.join(format!(
            ".aw-staging-{transaction}-{}-{index}",
            std::process::id()
        ));
        fixture.record(&staging, b"partial payload", 0o600);
    }

    let report = fixture.recover();
    assert_eq!((report.committed, report.rolled_back), (0, 0));
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert!(!directory.exists());
    assert!(fs::entries(&transactions).unwrap().is_empty());
    let again = fixture.recover();
    assert!(again.conflicts.is_empty());

    // The retired staging directory must not block later transactions.
    let removal = crate::store::plan::uninstall(&fixture.paths, &fixture.prefix, &[]).unwrap();
    assert!(removal.ready(), "{:?}", removal.problems);
    crate::store::plan::execute(&fixture.paths, &removal, &AtomicBool::new(false)).unwrap();
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert!(inventory.installations[0].packages.is_empty());
    assert!(inventory.doctor().unwrap().healthy());
}

#[test]
fn staging_cleanup_never_deletes_unrecognized_files() {
    let fixture = Fixture::new();
    super::super::apply(
        &fixture.paths,
        &fixture.changes(false),
        &AtomicBool::new(false),
    )
    .unwrap();
    let transactions = fixture.paths.root.join("transactions");
    let transaction = journal::new_id().unwrap();
    let directory = transactions.join(&transaction);
    fixture.directory(&directory);
    let staging = directory.join(format!(
        ".aw-staging-{transaction}-{}-0",
        std::process::id()
    ));
    fixture.record(&staging, b"partial payload", 0o600);
    let unrelated = directory.join("unrelated");
    fixture.record(&unrelated, b"foreign bytes", 0o600);

    let report = fixture.recover();
    assert_eq!(report.conflicts, vec![directory.clone()]);
    assert!(
        !staging.exists(),
        "cleanup must drop the owned staging file"
    );
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"foreign bytes");
}

#[test]
fn sigkill_between_journal_staging_creation_and_publication_is_retired() {
    let fixture = Fixture::new();
    fixture.kill_with_env(
        "journal-staged-unpublished",
        false,
        &[("AW_PACKAGE_TEST_FORCE_FALLBACK", "yes")],
    );
    let transactions = fixture.paths.root.join("transactions");
    let directories = fs::entries(&transactions).unwrap();
    assert_eq!(directories.len(), 1);
    let entries = fs::entries(&directories[0]).unwrap();
    assert!(entries.iter().all(|path| is_staging(path)), "{entries:?}");

    let report = fixture.recover();
    assert_eq!((report.committed, report.rolled_back), (0, 0));
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert!(!directories[0].exists());
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert!(inventory.installations.is_empty());
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    assert_eq!(fixture.recover().conflicts.len(), 0);
}

#[test]
fn sigkill_after_workspace_staging_creation_leaves_no_recovery_conflict() {
    let fixture = Fixture::new();
    fixture.kill_with_env(
        "staged-bytes-unpublished",
        false,
        &[("AW_PACKAGE_TEST_FORCE_FALLBACK", "yes")],
    );
    let transactions = fixture.paths.root.join("transactions");
    let directories = fs::entries(&transactions).unwrap();
    assert_eq!(directories.len(), 1);
    let entry = journal::read(&fixture.paths, &directories[0]).unwrap();
    let workspace = entry.workspace();
    let leftovers = fs::entries(&workspace).unwrap();
    assert!(
        leftovers.iter().any(|path| is_staging(path)),
        "{leftovers:?}"
    );
    assert!(matches!(
        Inventory::read(&fixture.paths, None),
        Err(Error::NeedsRecovery(_))
    ));

    let report = fixture.recover();
    assert_eq!((report.committed, report.rolled_back), (0, 1));
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert!(!directories[0].exists());
    assert!(!workspace.exists());
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert!(inventory.installations.is_empty());
    assert!(inventory.doctor().unwrap().healthy());
    drop(inventory);
    assert_eq!(fixture.recover().rolled_back, 0);
}

#[test]
fn staging_failures_leave_no_residue_and_keep_the_database_retryable() {
    for write in [true, false] {
        let fixture = Fixture::new();
        let failure = staging_failure(&fixture, write);
        assert!(
            matches!(
                &failure.error,
                Error::Io(error)
                    if error.raw_os_error()
                        == Some(if write { libc::ENOSPC } else { libc::EIO })
            ),
            "{:?}",
            failure.error
        );
        assert!(!failure.report.committed);
        let transactions = fixture.paths.root.join("transactions");
        assert!(fs::entries(&transactions).unwrap().is_empty());
        assert!(
            !fixture.prefix.exists(),
            "rolled back prefix must disappear"
        );
        let report = fixture.recover();
        assert_eq!((report.committed, report.rolled_back), (0, 0));
        assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
        let inventory = Inventory::read(&fixture.paths, None).unwrap();
        assert!(inventory.installations.is_empty());
        assert!(inventory.doctor().unwrap().healthy());
        drop(inventory);

        // A retry planned against the rolled-back database must succeed.
        let mut retry = fixture.changes(false);
        retry.database_before = journal::current_database(&fixture.paths).unwrap();
        super::super::apply(&fixture.paths, &retry, &AtomicBool::new(false)).unwrap();
        let inventory = Inventory::read(&fixture.paths, None).unwrap();
        assert_eq!(inventory.installations[0].packages.len(), 1);
    }
}

#[test]
fn replacement_workspace_bytes_survive_recovery() {
    let fixture = Fixture::new();
    fixture.kill_with_env(
        "staged-bytes-unpublished",
        false,
        &[("AW_PACKAGE_TEST_FORCE_FALLBACK", "yes")],
    );
    let transactions = fixture.paths.root.join("transactions");
    let directories = fs::entries(&transactions).unwrap();
    assert_eq!(directories.len(), 1);
    let entry = journal::read(&fixture.paths, &directories[0]).unwrap();
    let workspace = entry.workspace();
    assert!(
        fs::entries(&workspace)
            .unwrap()
            .iter()
            .any(|path| is_staging(path)),
        "the crash must leave a staging file behind"
    );

    let recorded = fs::directory_identity(&workspace).unwrap();
    let journaled = entry.workspace_identity.clone().unwrap();
    assert_eq!((recorded.dev, recorded.ino), (journaled.dev, journaled.ino));

    // Replace the workspace with a new inode that reuses a staging basename.
    // The replacement directory is allocated while the original still exists,
    // so the filesystem cannot hand the original inode number back to it.
    // Recovery must not delete bytes it cannot attribute to this transaction.
    let replacement = workspace.with_file_name(format!(".replacement-{}", entry.transaction_id));
    std::fs::create_dir(&replacement).unwrap();
    std::fs::remove_dir_all(&workspace).unwrap();
    std::fs::rename(&replacement, &workspace).unwrap();
    let replaced = fs::directory_identity(&workspace).unwrap();
    assert_ne!(
        (recorded.dev, recorded.ino),
        (replaced.dev, replaced.ino),
        "the replacement directory must use a new inode"
    );
    let foreign = workspace.join(format!(".aw-staging-{}-4242-7", entry.transaction_id));
    std::fs::write(&foreign, b"foreign bytes\n").unwrap();

    let report = fixture.recover();
    assert_eq!((report.committed, report.rolled_back), (0, 0));
    assert_eq!(report.conflicts, vec![workspace.clone()]);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign bytes\n");
    assert!(workspace.exists());

    // Recovery is idempotent: the same bytes still conflict on the next pass.
    let report = fixture.recover();
    assert_eq!(report.conflicts, vec![workspace]);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign bytes\n");
}
