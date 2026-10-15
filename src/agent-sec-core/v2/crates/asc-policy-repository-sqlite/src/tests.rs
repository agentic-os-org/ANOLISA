//! Real `SQLite` error and transaction boundaries, without a custom VFS.
use super::*;
use asc_policy_repository::BindingStateRepository;
use std::os::unix::fs::PermissionsExt;

thread_local! {
    static CRASH_BEFORE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn before_commit(transaction: &Transaction<'_>) {
    use std::io::Write as _;
    if CRASH_BEFORE_COMMIT.get() {
        let expected: i64 = std::env::var("ASC_UNCOMMITTED_INSTANCES")
            .unwrap()
            .parse()
            .unwrap();
        // Require the complete aggregate to be staged in this transaction before killing it.
        let bindings: i64 = transaction
            .query_row("SELECT count(*) FROM bindings", [], |row| row.get(0))
            .unwrap();
        let pins: i64 = transaction
            .query_row(
                "SELECT count(*) FROM scopes WHERE pinned_process_json IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bindings, expected);
        assert_eq!(pins, i64::from(expected == 1));
        println!("GATE:discovery_before_commit");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}

#[test]
fn uncommitted_discovery_child() {
    let Ok(path) = std::env::var("ASC_UNCOMMITTED_DATABASE") else {
        return;
    };
    let binding: asc_policy_types::binding::PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let count: u32 = std::env::var("ASC_UNCOMMITTED_INSTANCES")
        .unwrap()
        .parse()
        .unwrap();
    let instances: Vec<_> = (0..count)
        .map(|offset| {
            let mut process = binding.scope.process.clone();
            process.pid += offset;
            process
        })
        .collect();
    let repo = SqlitePolicyRepository::open(Path::new(&path)).unwrap();
    CRASH_BEFORE_COMMIT.set(true);
    repo.sync_scope_instances(&binding.scope.scope_id, &instances)
        .unwrap();
    panic!("discovery committed before the crash gate");
}

#[test]
fn sigkill_before_discovery_commit_rolls_back_pin_and_all_bindings() {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    for count in [1, 200] {
        let directory = directory();
        let path = directory.path().join("policy-state.db");
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        let binding: asc_policy_types::binding::PreparedBinding = serde_json::from_str(
            include_str!("../../asc-policy-types/tests/fixtures/prepared-binding.json"),
        )
        .unwrap();
        repo.put_policy(&binding.policy).unwrap();
        let scope = PreparedScope {
            scope_id: binding.scope.scope_id,
            selector: if count == 1 {
                binding.scope.selector
            } else {
                ScopeSelector::Process {
                    matcher: asc_policy_types::scope::ProcessMatcher::Name {
                        process_name: "worker".into(),
                    },
                }
            },
            policy_snapshots: vec![binding.policy],
            status: ScopeStatus::Active,
        };
        repo.put_scope(&scope).unwrap();
        let before = repo.scope_discovery_seed(&scope.scope_id).unwrap();
        drop(repo);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::uncommitted_discovery_child",
                "--nocapture",
            ])
            .env("ASC_UNCOMMITTED_DATABASE", &path)
            .env("ASC_UNCOMMITTED_INSTANCES", count.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if line == "GATE:discovery_before_commit" {
                    sender.send(()).unwrap();
                }
            }
        });
        let gate = receiver.recv_timeout(Duration::from_secs(10));
        let _ = child.kill();
        let status = child.wait().unwrap();
        reader.join().unwrap();
        gate.expect("child did not reach the uncommitted production transaction");
        assert_eq!(status.signal(), Some(9));
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        let after = repo.scope_discovery_seed(&scope.scope_id).unwrap();
        assert_eq!(after.scope, before.scope);
        assert_eq!(after.pinned_process, before.pinned_process);
        assert!(after.instances.is_empty());
        assert_eq!(repo.list_bindings(1000, 0).unwrap().total, 0);
        let instances: Vec<_> = (0..count)
            .map(|offset| {
                let mut process = binding.scope.process.clone();
                process.pid += offset;
                process
            })
            .collect();
        assert_eq!(
            repo.sync_scope_instances(&scope.scope_id, &instances)
                .unwrap()
                .len(),
            count as usize
        );
    }
}

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
