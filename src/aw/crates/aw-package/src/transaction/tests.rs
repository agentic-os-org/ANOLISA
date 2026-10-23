//! Real process death separates durable replay from in-process error cleanup.

mod capacity;
mod durability;

use super::{fs, journal, prepare, ChangeSet, SourceFile};
use crate::{
    database::{
        DatabasePaths, Installation, InstalledDatabase, InstalledPackage, Inventory, Manager,
        Registration, RegistrationPointer, Scope, SourceEvidence,
    },
    Component, Error,
};
use std::{
    collections::BTreeMap,
    fs as disk,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

struct Fixture {
    _directory: Option<tempfile::TempDir>,
    root: PathBuf,
    paths: DatabasePaths,
    prefix: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/package-transactions");
        disk::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        disk::set_permissions(directory.path(), disk::Permissions::from_mode(0o700)).unwrap();
        let mut fixture = Self::at(directory.path());
        fixture._directory = Some(directory);
        fixture
    }

    fn at(root: &Path) -> Self {
        let mut paths = DatabasePaths::user(Some(root), None).unwrap();
        paths.root = root.join("database");
        Self {
            _directory: None,
            root: root.into(),
            paths,
            prefix: root.join("install"),
        }
    }

    fn directory(&self, path: &Path) {
        let mut current = self.root.clone();
        for part in path.strip_prefix(&current).unwrap().components() {
            current.push(part);
            if !current.exists() {
                disk::create_dir(&current).unwrap();
                disk::set_permissions(&current, disk::Permissions::from_mode(0o700)).unwrap();
            }
        }
    }

    fn record(&self, path: &Path, bytes: impl AsRef<[u8]>, mode: u32) {
        self.directory(path.parent().unwrap());
        disk::write(path, bytes).unwrap();
        disk::set_permissions(path, disk::Permissions::from_mode(mode)).unwrap();
    }

    fn source(&self, name: &str, bytes: &[u8]) -> SourceFile {
        let source = self.root.join("bundle").join(name);
        self.record(&source, bytes, 0o755);
        SourceFile {
            source,
            metadata: fs::bytes_payload(bytes, 0o755),
        }
    }

    fn changes(&self, legacy: bool) -> ChangeSet {
        let source = self.source("tool", b"new tool\n");
        let files = BTreeMap::from([("bin/tool".into(), source.metadata.clone())]);
        let package = InstalledPackage {
            package_id: "example.tool".into(),
            version: "1.2.3".into(),
            os: "linux".into(),
            arch: std::env::consts::ARCH.into(),
            files,
            dependencies: Vec::new(),
            source: SourceEvidence {
                bundle_sha256: None,
                source_commit: None,
            },
            provider: None,
        };
        let mut changes = ChangeSet {
            prefix: self.prefix.clone(),
            database_before: None,
            database_after: InstalledDatabase {
                schema_version: 1,
                generation: 1,
                installations: vec![Installation {
                    installation_id: "install-one".into(),
                    prefix: self.prefix.clone(),
                    // SAFETY: geteuid only queries the fixture process identity.
                    owner_uid: unsafe { libc::geteuid() },
                    scope: Scope::User,
                    manager: Manager::Raw,
                    registration: Registration::Registered,
                    packages: vec![package],
                }],
            },
            installation_id: "install-one".into(),
            verify: BTreeMap::new(),
            writes: BTreeMap::from([("bin/tool".into(), source)]),
            removes: BTreeMap::new(),
            legacy: BTreeMap::new(),
            marker_before: None,
            marker_after: Some(
                serde_json::to_vec(
                    &RegistrationPointer::new(&self.paths, "install-one".into()).unwrap(),
                )
                .unwrap(),
            ),
            source_records: BTreeMap::new(),
        };
        if legacy {
            let old = self.prefix.join("bin/aw");
            self.record(&old, b"old core\n", 0o755);
            self.record(&self.prefix.join(".aw-install.lock"), b"", 0o600);
            let component = Component {
                format: 1,
                component: "aw-core".into(),
                version: "old-build".into(),
                source_commit: "historical-source".into(),
                os: "linux".into(),
                arch: std::env::consts::ARCH.into(),
                provider_protocol: "aw-provider/v1alpha1".into(),
                requires: BTreeMap::new(),
                files: BTreeMap::from([("bin/aw".into(), fs::bytes_payload(b"old core\n", 0o755))]),
            };
            let bytes = serde_json::to_vec_pretty(&component).unwrap();
            self.record(
                &self.prefix.join(".aw-packages/aw-core.json"),
                &bytes,
                0o644,
            );
            changes.verify = component.files.clone();
            changes.marker_before = Some(bytes.clone());
            changes.legacy.insert("aw-core.json".into(), bytes);
            changes.database_after.installations[0]
                .packages
                .push(InstalledPackage {
                    package_id: "aw-core".into(),
                    version: component.version,
                    os: component.os,
                    arch: component.arch,
                    files: component.files,
                    dependencies: Vec::new(),
                    source: SourceEvidence {
                        bundle_sha256: None,
                        source_commit: Some(component.source_commit),
                    },
                    provider: None,
                });
        }
        changes
    }

    fn kill(&self, phase: &str, legacy: bool) {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "transaction::tests::crash_child", "--nocapture"])
            .env("AW_PACKAGE_TRANSACTION_CHILD", &self.root)
            .env(
                "AW_PACKAGE_TRANSACTION_LEGACY",
                if legacy { "yes" } else { "no" },
            )
            .env("AW_PACKAGE_TEST_KILL_AT", phase)
            .env(
                "AW_PACKAGE_TRANSACTION_SYSTEM_CLEANUP",
                if phase == "lock-readable" {
                    "yes"
                } else {
                    "no"
                },
            )
            .spawn()
            .unwrap();
        let mut guard = ChildGuard(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            if let Some(status) = guard.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "checkpoint child exceeded deadline: {phase}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "checkpoint {phase} was not reached"
        );
    }

    fn recover(&self) -> super::RecoveryReport {
        super::recover(&self.paths, &AtomicBool::new(false)).unwrap()
    }
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("AW_PACKAGE_TRANSACTION_CHILD") else {
        return;
    };
    let fixture = Fixture::at(Path::new(&root));
    if std::env::var("AW_PACKAGE_TRANSACTION_SYSTEM_CLEANUP")
        .ok()
        .as_deref()
        == Some("yes")
    {
        system_cleanup(&fixture);
        panic!("requested system-cleanup checkpoint was not reached");
    }
    let legacy = std::env::var("AW_PACKAGE_TRANSACTION_LEGACY").unwrap() == "yes";
    let changes = fixture.changes(legacy);
    super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).unwrap();
    panic!("requested crash checkpoint was not reached");
}

#[test]
fn sigkill_before_database_commit_rolls_back_and_replay_is_idempotent() {
    let fixture = Fixture::new();
    fixture.kill("precommit", false);
    assert!(fixture.prefix.join("bin/tool").exists());
    let report = fixture.recover();
    assert_eq!(report.rolled_back, 1);
    assert!(report.conflicts.is_empty());
    assert!(!fixture.prefix.exists());
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert_eq!(inventory.generation, Some(0));
    assert!(inventory.installations.is_empty());
    drop(inventory);
    assert_eq!(fixture.recover().rolled_back, 0);
}

#[test]
fn sigkill_after_commit_and_before_cleanup_finishes_without_undoing_payload() {
    for phase in ["postcommit", "precleanup"] {
        let fixture = Fixture::new();
        fixture.kill(phase, false);
        assert!(matches!(
            Inventory::read(&fixture.paths, None),
            Err(Error::NeedsRecovery(_))
        ));
        let report = fixture.recover();
        assert_eq!(report.committed, 1, "{phase}");
        assert!(report.conflicts.is_empty());
        assert_eq!(
            disk::read(fixture.prefix.join("bin/tool")).unwrap(),
            b"new tool\n"
        );
        let inventory = Inventory::read(&fixture.paths, None).unwrap();
        assert_eq!(inventory.generation, Some(1));
        assert_eq!(
            inventory.installations[0].packages[0].package_id,
            "example.tool"
        );
        drop(inventory);
        assert_eq!(fixture.recover().committed, 0);
    }
}

#[test]
fn legacy_gate_is_present_after_sigkill_and_restored_only_after_rollback() {
    let fixture = Fixture::new();
    fixture.kill("gated", true);
    let path = fixture.prefix.join(".aw-packages/aw-core.json");
    let gate = disk::read(&path).unwrap();
    assert!(serde_json::from_slice::<Component>(&gate).is_err());
    assert!(serde_json::from_slice::<RegistrationPointer>(&gate).is_ok());
    let attempt = fs::entries(&fixture.paths.root.join("legacy/install-one"))
        .unwrap()
        .remove(0);
    let archived = disk::read(attempt.join("aw-core.json")).unwrap();
    assert_eq!(fixture.recover().rolled_back, 1);
    assert_eq!(disk::read(path).unwrap(), archived);
    assert_eq!(fixture.recover().rolled_back, 0);
    assert!(!fixture.prefix.join("bin/tool").exists());
}

#[test]
fn recovery_keeps_user_replacements_and_keeps_the_old_writer_closed() {
    let fixture = Fixture::new();
    fixture.kill("precommit", true);
    let tool = fixture.prefix.join("bin/tool");
    disk::remove_file(&tool).unwrap();
    fixture.record(&tool, b"user replacement\n", 0o755);
    let report = fixture.recover();
    assert!(report.conflicts.contains(&tool));
    assert_eq!(disk::read(&tool).unwrap(), b"user replacement\n");
    let marker = disk::read(fixture.prefix.join(".aw-packages/aw-core.json")).unwrap();
    assert!(serde_json::from_slice::<RegistrationPointer>(&marker).is_ok());
    assert!(!fixture.recover().conflicts.is_empty());
    assert!(matches!(
        Inventory::read(&fixture.paths, None),
        Err(Error::NeedsRecovery(_))
    ));
}

#[test]
fn execution_rechecks_source_and_database_before_mutating_prefix() {
    let fixture = Fixture::new();
    let changes = fixture.changes(false);
    disk::write(
        &changes.writes["bin/tool"].source,
        b"changed after planning\n",
    )
    .unwrap();
    assert!(super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).is_err());
    assert!(!fixture.prefix.exists());
    assert!(Inventory::read(&fixture.paths, None)
        .unwrap()
        .installations
        .is_empty());

    let changes = fixture.changes(false);
    let mut stale = changes.clone();
    stale.database_before = Some(disk::read(fixture.paths.installed()).unwrap());
    let mut database: InstalledDatabase =
        serde_json::from_slice(stale.database_before.as_ref().unwrap()).unwrap();
    database.generation = 9;
    disk::write(
        fixture.paths.installed(),
        serde_json::to_vec(&database).unwrap(),
    )
    .unwrap();
    assert!(super::apply(&fixture.paths, &stale, &AtomicBool::new(false)).is_err());
    assert!(!fixture.prefix.exists());
}

#[test]
fn prefix_lock_contention_is_reported_as_in_use() {
    let fixture = Fixture::new();
    let changes = fixture.changes(true);
    let _lock = fs::lock(&fixture.prefix.join(".aw-install.lock"), false).unwrap();
    let failure = super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(failure.error, Error::InUse(_)));
    assert!(!fixture.prefix.join("bin/tool").exists());
}

#[test]
fn unrecorded_created_prefix_is_retained_as_a_recovery_conflict() {
    let fixture = Fixture::new();
    let changes = fixture.changes(false);
    let prepared = prepare::begin(&fixture.paths, &changes).unwrap();
    fixture.directory(&fixture.prefix);
    fixture.record(&fixture.prefix.join("user.txt"), b"unknown owner", 0o600);
    drop(prepared);
    let report = fixture.recover();
    assert!(report.conflicts.contains(&fixture.prefix));
    assert_eq!(
        disk::read(fixture.prefix.join("user.txt")).unwrap(),
        b"unknown owner"
    );
}

#[test]
fn actual_database_bytes_decide_commit_even_without_a_phase_marker() {
    let fixture = Fixture::new();
    fixture.kill("postcommit", false);
    let directory = fs::entries(&fixture.paths.root.join("transactions"))
        .unwrap()
        .remove(0);
    let entry = journal::read(&fixture.paths, &directory).unwrap();
    assert!(super::replay::decide(&fixture.paths, &entry).unwrap());
    disk::write(
        fixture.paths.installed(),
        entry.before_database.as_ref().unwrap(),
    )
    .unwrap();
    assert!(!super::replay::decide(&fixture.paths, &entry).unwrap());
    assert_eq!(fixture.recover().rolled_back, 1);
    assert!(!fixture.prefix.exists());
}

#[test]
fn cleanup_retry_keeps_unknown_files_without_losing_its_recovery_path() {
    let fixture = Fixture::new();
    fixture.kill("precommit", false);
    let unknown = fixture.prefix.join("notes.txt");
    fixture.record(&unknown, b"keep my file", 0o600);
    let first = fixture.recover();
    assert!(first.conflicts.contains(&fixture.prefix));
    assert_eq!(disk::read(&unknown).unwrap(), b"keep my file");
    assert!(!fixture.recover().conflicts.is_empty());
    disk::remove_file(unknown).unwrap();
    assert_eq!(fixture.recover().rolled_back, 1);
    assert!(!fixture.prefix.exists());
    assert_eq!(fixture.recover().rolled_back, 0);
}

#[test]
fn system_database_permissions_allow_readers_without_exposing_journals() {
    let fixture = Fixture::new();
    let mut paths = DatabasePaths::system();
    paths.root = fixture.paths.root.clone();
    let changes = fixture.changes(false);
    // Exercise bootstrap in a private fixture, without granting public system-write scope.
    assert!(prepare::initialize(&paths, &changes).unwrap());
    assert_eq!(disk::metadata(&paths.root).unwrap().mode() & 0o777, 0o755);
    assert_eq!(
        disk::metadata(paths.root.join("transactions"))
            .unwrap()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        disk::metadata(paths.root.join("lock")).unwrap().mode() & 0o777,
        0o644
    );
    let journal = paths.root.join("transactions/private");
    fs::mkdir(&journal, 0o700).unwrap();
    assert_eq!(disk::metadata(journal).unwrap().mode() & 0o777, 0o700);
}

fn system_cleanup(fixture: &Fixture) {
    let _database = fs::lock(&fixture.paths.root.join("lock"), false).unwrap();
    let _prefix = fs::lock(&fixture.prefix.join(".aw-install.lock"), false).unwrap();
    let directory = fs::entries(&fixture.paths.root.join("transactions"))
        .unwrap()
        .remove(0);
    let entry = journal::read(&fixture.paths, &directory).unwrap();
    assert!(super::replay::decide(&fixture.paths, &entry).unwrap());
    let mut system = DatabasePaths::system();
    system.root = fixture.paths.root.clone();
    // Exercise the private scope-specific finalizer on a local committed fixture. The public
    // system entrypoint still requires root and the standard database and locator paths.
    assert!(super::replay::finish(&system, &entry, true)
        .unwrap()
        .is_empty());
}

#[test]
fn legacy_lock_permissions_remain_private_when_precommit_changes_roll_back() {
    let fixture = Fixture::new();
    fixture.kill("precommit", true);
    let lock = fixture.prefix.join(".aw-install.lock");
    assert_eq!(disk::metadata(&lock).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fixture.recover().rolled_back, 1);
    assert_eq!(disk::metadata(lock).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn committed_system_lock_permissions_survive_interrupted_cleanup_and_retry() {
    let fixture = Fixture::new();
    fixture.kill("postcommit", true);
    let lock = fixture.prefix.join(".aw-install.lock");
    assert_eq!(disk::metadata(&lock).unwrap().mode() & 0o777, 0o600);
    let before = fs::identity(&lock).unwrap();
    fixture.kill("lock-readable", true);
    assert_eq!(disk::metadata(&lock).unwrap().mode() & 0o777, 0o644);
    assert_eq!(fs::identity(&lock).unwrap().ino, before.ino);
    system_cleanup(&fixture);
    assert_eq!(disk::metadata(lock).unwrap().mode() & 0o777, 0o644);
    assert_eq!(
        disk::read(fixture.prefix.join("bin/tool")).unwrap(),
        b"new tool\n"
    );
    assert_eq!(fixture.recover().committed, 0);
}

#[test]
fn private_nonempty_or_replaced_locks_are_never_made_public() {
    let fixture = Fixture::new();
    fixture.directory(&fixture.prefix);
    let lock = fixture.prefix.join(".aw-install.lock");
    disk::write(&lock, b"private unknown bytes").unwrap();
    disk::set_permissions(&lock, disk::Permissions::from_mode(0o600)).unwrap();
    let before = fs::identity(&lock).unwrap();
    let mut after = before.clone();
    after.mode = 0o644;
    assert!(fs::make_readable(&lock, &before, &after).is_err());
    assert_eq!(disk::metadata(&lock).unwrap().mode() & 0o777, 0o600);
    disk::remove_file(&lock).unwrap();
    disk::write(&lock, b"different private bytes").unwrap();
    disk::set_permissions(&lock, disk::Permissions::from_mode(0o600)).unwrap();
    assert!(!fs::make_readable(&lock, &before, &after).unwrap());
    assert_eq!(disk::metadata(lock).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn journal_creation_gap_retires_only_its_empty_directory_and_allows_new_operations() {
    let fixture = Fixture::new();
    fixture.kill("journal-directory-created", false);
    assert!(!fixture.prefix.exists());
    assert!(matches!(
        Inventory::read(&fixture.paths, None),
        Err(Error::NeedsRecovery(_))
    ));
    let first = fixture.recover();
    assert_eq!((first.committed, first.rolled_back), (0, 0));
    assert!(first.conflicts.is_empty());
    assert!(fixture.recover().conflicts.is_empty());
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert_eq!(inventory.generation, Some(0));
    assert!(inventory.installations.is_empty());
    drop(inventory);
    let mut changes = fixture.changes(false);
    changes.database_before = Some(disk::read(fixture.paths.installed()).unwrap());
    super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        disk::read(fixture.prefix.join("bin/tool")).unwrap(),
        b"new tool\n"
    );
}

#[test]
fn final_journal_unlink_gap_preserves_committed_payload_and_recovery_is_idempotent() {
    let fixture = Fixture::new();
    fixture.kill("journal-file-removed", false);
    assert!(matches!(
        Inventory::read(&fixture.paths, None),
        Err(Error::NeedsRecovery(_))
    ));
    let first = fixture.recover();
    assert_eq!((first.committed, first.rolled_back), (0, 0));
    assert!(first.conflicts.is_empty());
    let second = fixture.recover();
    assert_eq!((second.committed, second.rolled_back), (0, 0));
    assert!(second.conflicts.is_empty());
    assert_eq!(
        disk::read(fixture.prefix.join("bin/tool")).unwrap(),
        b"new tool\n"
    );
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert_eq!(inventory.generation, Some(1));
    assert_eq!(
        inventory.installations[0].packages[0].package_id,
        "example.tool"
    );
}

#[test]
fn journal_less_unknown_files_names_and_symlinks_are_preserved_as_conflicts() {
    let fixture = Fixture::new();
    fixture.kill("journal-directory-created", false);
    let transactions = fixture.paths.root.join("transactions");
    let directory = fs::entries(&transactions).unwrap().remove(0);
    let unknown = directory.join("user.txt");
    fixture.record(&unknown, b"unknown transaction bytes", 0o600);
    let unrelated = transactions.join("not-a-transaction");
    fs::mkdir(&unrelated, 0o700).unwrap();
    let target = fixture.root.join("other-directory");
    fixture.directory(&target);
    let alias = transactions.join("txn-1-2-3");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    for _ in 0..2 {
        let report = fixture.recover();
        assert_eq!((report.committed, report.rolled_back), (0, 0));
        assert!(report.conflicts.contains(&directory));
        assert!(report.conflicts.contains(&unrelated));
        assert!(report.conflicts.contains(&alias));
    }
    assert_eq!(disk::read(unknown).unwrap(), b"unknown transaction bytes");
    assert!(unrelated.is_dir());
    assert!(alias.is_symlink());
    assert!(target.is_dir());
}

#[test]
fn a_rolled_back_migration_does_not_reserve_future_legacy_record_bytes() {
    let fixture = Fixture::new();
    fixture.kill("gated", true);
    assert_eq!(fixture.recover().rolled_back, 1);
    let archive = fixture.paths.root.join("legacy/install-one");
    let original_attempt = fs::entries(&archive).unwrap().remove(0);
    let original_bytes = disk::read(original_attempt.join("aw-core.json")).unwrap();

    let mut changes = fixture.changes(true);
    let mut record: Component =
        serde_json::from_slice(changes.marker_before.as_ref().unwrap()).unwrap();
    record.version = "old-build-r2".into();
    crate::database::validate_legacy_component(&record).unwrap();
    let changed_bytes = serde_json::to_vec(&record).unwrap();
    assert_ne!(changed_bytes, original_bytes);
    disk::write(
        fixture.prefix.join(".aw-packages/aw-core.json"),
        &changed_bytes,
    )
    .unwrap();
    changes.marker_before = Some(changed_bytes.clone());
    changes
        .legacy
        .insert("aw-core.json".into(), changed_bytes.clone());
    changes.database_after.installations[0]
        .packages
        .iter_mut()
        .find(|package| package.package_id == "aw-core")
        .unwrap()
        .version = record.version;
    changes.database_before = Some(disk::read(fixture.paths.installed()).unwrap());
    assert_eq!(changes.installation_id, "install-one");
    super::apply(&fixture.paths, &changes, &AtomicBool::new(false)).unwrap();

    let attempts = fs::entries(&archive).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        disk::read(original_attempt.join("aw-core.json")).unwrap(),
        original_bytes
    );
    let latest = attempts
        .into_iter()
        .find(|path| path != &original_attempt)
        .unwrap();
    assert_eq!(
        disk::read(latest.join("aw-core.json")).unwrap(),
        changed_bytes
    );
    let inventory = Inventory::read(&fixture.paths, None).unwrap();
    assert_eq!(inventory.installations[0].installation_id, "install-one");
    assert_eq!(
        inventory.installations[0]
            .packages
            .iter()
            .find(|package| package.package_id == "aw-core")
            .unwrap()
            .version,
        "old-build-r2"
    );
}
