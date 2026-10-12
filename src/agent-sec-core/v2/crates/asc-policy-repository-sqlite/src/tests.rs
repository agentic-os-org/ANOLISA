//! Real `SQLite` error and transaction boundaries, without a custom VFS.
use super::*;
use asc_policy_repository::BindingStateRepository;
use std::os::unix::fs::PermissionsExt;

fn directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[test]
fn lease_verification_rejects_replaced_database() {
    for symlink in [false, true] {
        let dir = directory();
        let path = dir.path().canonicalize().unwrap().join("policy-state.db");
        let lease = DatabaseLease::acquire(&path).unwrap();
        lease.verify().unwrap();
        let saved = path.with_file_name("saved.db");
        std::fs::rename(&path, &saved).unwrap();
        if symlink {
            std::os::unix::fs::symlink(&saved, &path).unwrap();
        } else {
            std::fs::copy(&saved, &path).unwrap();
        }
        assert!(matches!(lease.verify(), Err(RepositoryError::UnsafePath)));
    }
}

#[test]
fn lease_verification_rejects_replaced_directory() {
    let dir = directory();
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("state/policy-state.db");
    let lease = DatabaseLease::acquire(&path).unwrap();
    lease.verify().unwrap();
    std::fs::rename(root.join("state"), root.join("saved-state")).unwrap();
    let replacement = DatabaseLease::acquire(&path).unwrap();
    replacement.verify().unwrap();
    assert!(matches!(lease.verify(), Err(RepositoryError::UnsafePath)));
}

#[test]
fn readonly_probe_fails_and_reopened_connection_recovers() {
    let dir = directory();
    let repo = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    repo.storage
        .lock()
        .unwrap()
        .connection
        .as_ref()
        .unwrap()
        .execute_batch("PRAGMA query_only=ON")
        .unwrap();
    assert_eq!(repo.check_writable(), Err(StoreError::ReadOnly));
    assert_eq!(repo.check_writable(), Ok(()));
}

#[test]
fn full_and_io_errors_rollback_and_do_not_leak_transactions() {
    let dir = directory();
    let repo = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    {
        let guard = repo.storage.lock().unwrap();
        let db = guard.connection.as_ref().unwrap();
        let pages: i64 = db
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        db.pragma_update(None, "max_page_count", pages).unwrap();
    }
    let result = repo.access(true, |tx| {
        tx.execute(
            "INSERT INTO policies VALUES ('large',1,?1)",
            [serde_json::to_string(&"x".repeat(1024 * 1024)).unwrap()],
        )?;
        Ok(())
    });
    assert!(matches!(
        result,
        Err(RepositoryError::Storage(StoreError::Full))
    ));
    let result: Result<(), RepositoryError> = repo.access(true, |tx| {
        tx.execute("INSERT INTO policies VALUES ('uncommitted',1,NULL)", [])?;
        Err(StoreError::Io.into())
    });
    assert!(matches!(
        result,
        Err(RepositoryError::Storage(StoreError::Io))
    ));
    assert_eq!(repo.check_writable(), Ok(()));
    repo.access(false, |tx| {
        assert_eq!(
            tx.query_row("SELECT count(*) FROM policies", [], |r| r.get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn transaction_panic_rolls_back_without_poisoning_repository() {
    let dir = directory();
    let repo = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        repo.access::<()>(true, |tx| {
            tx.execute("INSERT INTO policies VALUES ('uncommitted',1,NULL)", [])?;
            panic!("injected transaction panic");
        })
    }));
    assert!(result.is_err());
    assert_eq!(repo.check_writable(), Ok(()));
    repo.access(false, |tx| {
        assert_eq!(
            tx.query_row("SELECT count(*) FROM policies", [], |r| r.get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
}
