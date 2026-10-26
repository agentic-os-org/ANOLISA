//! Startup path selection must preserve existing Policy state across permission changes.

use super::*;
use crate::SqlitePolicyRepository;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap();
    let primary = root.path().join("log/agent-sec/policy-state.db");
    let fallback = root.path().join("lib/policy/policy-state.db");
    for path in [&primary, &fallback] {
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    (root, primary, fallback)
}

fn group_writable_ancestor(primary: &Path) -> PathBuf {
    let log = primary.parent().unwrap().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o775)).unwrap();
    log
}

#[test]
fn dproc_023_selection_checks_both_existing_databases_before_initialization() {
    for primary_safe in [true, false] {
        for primary_exists in [false, true] {
            for fallback_exists in [false, true] {
                let (_root, primary, fallback) = fixture();
                for (path, exists) in [(&primary, primary_exists), (&fallback, fallback_exists)] {
                    if exists {
                        drop(SqlitePolicyRepository::open(path).unwrap());
                    }
                }
                if !primary_safe {
                    group_writable_ancestor(&primary);
                }
                let before: Vec<_> = [&primary, &fallback]
                    .into_iter()
                    .map(|path| {
                        std::fs::metadata(path)
                            .ok()
                            .map(|m| (m.dev(), m.ino(), std::fs::read(path).unwrap()))
                    })
                    .collect();
                let result = select_policy_database_path(&primary, Some(&fallback));
                let conflict = if primary_safe {
                    fallback_exists
                } else {
                    primary_exists
                };
                if conflict {
                    let error = result.unwrap_err().to_string();
                    assert!(error.contains(&primary.display().to_string()), "{error}");
                    assert!(error.contains(&fallback.display().to_string()), "{error}");
                    assert!(
                        error.contains(if primary_exists && fallback_exists {
                            "exist at both A"
                        } else {
                            "refusing to bypass existing Policy state"
                        }),
                        "{error}"
                    );
                } else {
                    assert_eq!(
                        result.unwrap(),
                        if primary_safe {
                            primary.clone()
                        } else {
                            fallback.clone()
                        }
                    );
                }
                let after: Vec<_> = [&primary, &fallback]
                    .into_iter()
                    .map(|path| {
                        std::fs::metadata(path)
                            .ok()
                            .map(|m| (m.dev(), m.ino(), std::fs::read(path).unwrap()))
                    })
                    .collect();
                assert_eq!(before, after);
            }
        }
    }
}

#[test]
fn fallback_state_is_not_bypassed_when_primary_permissions_are_restored() {
    let (_root, primary, fallback) = fixture();
    let ancestor = group_writable_ancestor(&primary);
    let selected = select_policy_database_path(&primary, Some(&fallback)).unwrap();
    assert_eq!(selected, fallback);
    drop(SqlitePolicyRepository::open(&selected).unwrap());
    std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = select_policy_database_path(&primary, Some(&fallback))
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exists"), "{error}");
    assert!(error.contains("A's directory satisfies"), "{error}");
    assert!(!primary.exists());
    SqlitePolicyRepository::open(&fallback).unwrap();
}

#[test]
fn owned_primary_leaf_is_repaired_and_remains_selected_across_restarts() {
    for existing in [false, true] {
        for explicit in [false, true] {
            let (_root, primary, fallback) = fixture();
            if existing {
                drop(SqlitePolicyRepository::open(&primary).unwrap());
            }
            let leaf = primary.parent().unwrap();
            let ancestor = leaf.parent().unwrap();
            let ancestor_mode = std::fs::metadata(ancestor).unwrap().mode();
            std::fs::set_permissions(leaf, std::fs::Permissions::from_mode(0o755)).unwrap();
            let fallback_path = (!explicit).then_some(fallback.as_path());
            assert_eq!(
                select_policy_database_path(&primary, fallback_path).unwrap(),
                primary
            );
            assert_eq!(std::fs::metadata(leaf).unwrap().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(ancestor).unwrap().mode(), ancestor_mode);
            drop(SqlitePolicyRepository::open(&primary).unwrap());
            let before = std::fs::read(&primary).unwrap();
            let identity = std::fs::metadata(&primary).unwrap().ino();
            assert_eq!(
                select_policy_database_path(&primary, fallback_path).unwrap(),
                primary
            );
            drop(SqlitePolicyRepository::open(&primary).unwrap());
            assert_eq!(std::fs::metadata(&primary).unwrap().ino(), identity);
            assert_eq!(std::fs::read(&primary).unwrap(), before);
            assert!(!fallback.exists());
        }
    }
}

#[test]
fn conflicting_databases_are_rejected_before_repairing_leaf_permissions() {
    let (_root, primary, fallback) = fixture();
    for path in [&primary, &fallback] {
        drop(SqlitePolicyRepository::open(path).unwrap());
    }
    let leaf = primary.parent().unwrap();
    std::fs::set_permissions(leaf, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(select_policy_database_path(&primary, Some(&fallback)).is_err());
    assert_eq!(std::fs::metadata(leaf).unwrap().mode() & 0o777, 0o755);
}

#[test]
fn foreign_owned_primary_leaf_is_rejected_without_fallback() {
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }
    let (_root, primary, fallback) = fixture();
    let leaf = primary.parent().unwrap();
    std::os::unix::fs::chown(leaf, Some(1000), None).unwrap();
    let error = select_policy_database_path(&primary, Some(&fallback))
        .unwrap_err()
        .to_string();
    assert!(error.contains(&leaf.display().to_string()), "{error}");
    assert!(error.contains("actual uid=1000"), "{error}");
    assert!(!fallback.exists());
}

#[test]
fn fallback_preflight_does_not_create_directories_or_chmod_the_primary() {
    let (_root, primary, fallback) = fixture();
    let ancestor = group_writable_ancestor(&primary);
    std::fs::remove_dir(fallback.parent().unwrap()).unwrap();
    assert_eq!(
        select_policy_database_path(&primary, Some(&fallback)).unwrap(),
        fallback
    );
    assert!(!fallback.parent().unwrap().exists());
    assert_eq!(std::fs::metadata(&ancestor).unwrap().mode() & 0o777, 0o775);
    drop(SqlitePolicyRepository::open(&fallback).unwrap());
    assert_eq!(
        std::fs::metadata(fallback.parent().unwrap())
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(std::fs::metadata(&fallback).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn explicit_directory_is_strict_and_diagnostics_identify_the_unsafe_ancestor() {
    let (_root, primary, _fallback) = fixture();
    let ancestor = group_writable_ancestor(&primary);
    let error = select_policy_database_path(&primary, None)
        .unwrap_err()
        .to_string();
    for expected in [
        ancestor.display().to_string(),
        "mode=0775".into(),
        "uid=".into(),
        "gid=".into(),
        "without group/other write".into(),
    ] {
        assert!(error.contains(&expected), "{error}");
    }
    assert!(!primary.exists());
}

#[test]
fn unsafe_fallback_reports_both_directory_failures_without_creating_state() {
    let (_root, primary, fallback) = fixture();
    group_writable_ancestor(&primary);
    std::fs::set_permissions(
        fallback.parent().unwrap().parent().unwrap(),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    let error = select_policy_database_path(&primary, Some(&fallback))
        .unwrap_err()
        .to_string();
    for expected in [
        "neither Policy database directory",
        "mode=0775",
        "mode=0777",
        "without group/other write",
        "no database was created",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    assert!(!primary.exists());
    assert!(!fallback.exists());
}

#[test]
fn symlink_and_orphaned_sidecars_are_not_treated_as_absent_databases() {
    for symlink in [true, false] {
        let (_root, primary, fallback) = fixture();
        if symlink {
            let alias = primary.parent().unwrap().with_file_name("saved");
            std::fs::rename(primary.parent().unwrap(), &alias).unwrap();
            std::os::unix::fs::symlink(&alias, primary.parent().unwrap()).unwrap();
        } else {
            std::fs::write(
                primary.with_file_name("policy-state.db-wal"),
                "retained WAL",
            )
            .unwrap();
        }
        let error = select_policy_database_path(&primary, Some(&fallback))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&primary.parent().unwrap().display().to_string()),
            "{error}"
        );
        assert!(!fallback.exists());
    }
}

#[test]
fn corrupt_or_locked_selected_database_never_initializes_the_fallback() {
    for locked in [true, false] {
        let (_root, primary, fallback) = fixture();
        let repository = SqlitePolicyRepository::open(&primary).unwrap();
        let lease = repository.lease();
        drop(repository);
        if !locked {
            drop(lease);
            std::fs::write(&primary, "not a SQLite database").unwrap();
        }
        let selected = select_policy_database_path(&primary, Some(&fallback)).unwrap();
        assert_eq!(selected, primary);
        assert!(SqlitePolicyRepository::open(&selected).is_err());
        assert!(!fallback.exists());
    }
}
