//! Real `SQLite` error and transaction boundaries, without a custom VFS.
use super::*;
use asc_policy_repository::BindingStateRepository;
use std::os::unix::fs::PermissionsExt;

#[cfg(feature = "test-support")]
#[test]
fn temporary_databases_are_isolated_and_removed_after_repository_drop() {
    let first = SqlitePolicyRepository::temporary().unwrap();
    let second = SqlitePolicyRepository::temporary().unwrap();
    let first_path = first.lease.path.clone();
    let second_path = second.lease.path.clone();
    assert_ne!(first_path, second_path);
    assert!(first_path.is_file());
    assert!(second_path.is_file());
    drop(first);
    assert!(!first_path.parent().unwrap().exists());
    assert!(second_path.is_file());
    second.check_writable().unwrap();
    drop(second);
    assert!(!second_path.parent().unwrap().exists());
}

#[cfg(feature = "test-support")]
#[test]
fn before_commit_hook_observes_staged_writes_only_once() {
    thread_local! {
        static CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    let pap = PapService::new(Arc::new(SqlitePolicyRepository::temporary().unwrap()));
    // The crash test never returns from its hook, so check one-shot behavior here.
    SqlitePolicyRepository::before_next_commit_for_test(|transaction| {
        assert!(!transaction.is_autocommit());
        let staged: i64 = transaction
            .query_row("SELECT count(*) FROM policies", [], |row| row.get(0))
            .unwrap();
        assert_eq!(staged, 1);
        CALLS.set(CALLS.get() + 1);
    });

    assert_eq!(pap.list_policies(10, 0).unwrap().total, 0);
    for expected in [1, 2] {
        pap.create_policy(
            "protect files",
            &file_policy(vec!["/workspace/important".into()]),
        )
        .unwrap();
        assert_eq!(CALLS.get(), 1);
        assert_eq!(pap.list_policies(10, 0).unwrap().total, expected);
    }
}

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
        assert!(matches!(
            lease.verify(),
            Err(RepositoryError::UnsafePath { .. })
        ));
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
    assert!(matches!(
        lease.verify(),
        Err(RepositoryError::UnsafePath { .. })
    ));
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

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

use asc_foundation_types::ResourceId;
use asc_pap::{PapRepository, PapService};
use asc_policy_types::{
    binding::BindingStatus,
    process_discovery::ProcessIdentity,
    scope::{PreparedScope, ScopeSelector, ScopeStatus},
};
use std::sync::Arc;

#[test]
fn oversized_policy_and_assignment_are_rejected_before_commit() {
    let dir = directory();
    let repository =
        Arc::new(SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap());
    let pap = PapService::new(repository.clone());
    let huge = file_policy(
        (0..400)
            .map(|n| format!("/{n}{}", "x".repeat(3000)))
            .collect(),
    );
    assert!(matches!(
        pap.create_policy("large", &huge),
        Err(PapError::InvalidPolicy(_))
    ));
    assert_eq!(pap.list_policies(10, 0).unwrap().total, 0);
    let medium = file_policy(
        (0..200)
            .map(|n| format!("/{n}{}", "x".repeat(3000)))
            .collect(),
    );
    let a = pap.create_policy("a", &medium).unwrap();
    let b = pap.create_policy("b", &medium).unwrap();
    let scope = PreparedScope {
        scope_id: ResourceId::new("oversized").unwrap(),
        selector: ScopeSelector::Pid { pid: 42 },
        policy_snapshots: vec![a, b],
        owner_uid: None,
        status: ScopeStatus::Active,
    };
    assert!(matches!(
        repository.put_scope(&scope),
        Err(PapError::InvalidScope(_))
    ));
    assert_eq!(pap.list_scopes(10, 0).unwrap().total, 0);
}

#[test]
fn repository_runs_the_current_pap_request_slice_without_a_reconciler() {
    let dir = directory();
    let repository =
        Arc::new(SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap());
    let pap = PapService::new(Arc::clone(&repository));
    let policy = pap
        .create_policy(
            "protect files",
            &file_policy(vec!["/workspace/important".to_owned()]),
        )
        .unwrap();
    let scope = PreparedScope {
        scope_id: ResourceId::new("scope").unwrap(),
        selector: ScopeSelector::Pid { pid: 4242 },
        policy_snapshots: vec![policy],
        owner_uid: None,
        status: ScopeStatus::Active,
    };
    repository.put_scope(&scope).unwrap();
    let binding = repository
        .sync_scope_instances(
            &scope.scope_id,
            &[ProcessIdentity {
                boot_id: "boot".into(),
                pid_namespace: "pid:[42]".into(),
                pid: 4242,
                start_time: 99,
            }],
        )
        .unwrap()
        .remove(0);

    assert_eq!(binding.status, BindingStatus::PendingApply);
    assert_eq!(pap.list_policies(10, 0).unwrap().total, 1);
    assert_eq!(pap.list_scopes(10, 0).unwrap().total, 1);
    assert_eq!(pap.list_bindings(10, 0).unwrap().items, [binding.binding]);
    drop(pap);
    drop(repository);
    let reopened = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    assert_eq!(reopened.list_policies(10, 0).unwrap().total, 1);
    assert_eq!(reopened.list_scopes(10, 0).unwrap().total, 1);
    assert_eq!(reopened.list_bindings(10, 0).unwrap().total, 1);
}

#[test]
fn page_byte_budget_preserves_count_and_allows_progress() {
    let records = vec!["x".repeat(1024 * 1024); 4];
    let first = records::page(4, records.iter().take(2).cloned().map(Ok)).unwrap();
    assert_eq!(first.total, 4);
    assert_eq!(first.items.len(), 2);
    let bounded = records::page(4, records.iter().cloned().map(Ok)).unwrap();
    assert_eq!(bounded.items, first.items);
    let second = records::page(4, records.into_iter().skip(2).map(Ok)).unwrap();
    assert_eq!(second.items.len(), 2);
}
