use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{PapError, PapRepository};
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_repository::*;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::{
    binding::{BindingStatus, PreparedBinding},
    scope::{PreparedScope, ScopeStatus},
    target::{Presence, TargetRef},
};
use std::os::unix::fs::PermissionsExt;

#[test]
fn repository_retains_sqlite_wal_locks_like_a_plain_connection() {
    use std::os::unix::fs::MetadataExt;

    let dir = private_dir();
    let holds_deadman_lock = |name: &str| {
        let inode = std::fs::metadata(dir.path().join(name))
            .unwrap()
            .ino()
            .to_string();
        let pid = std::process::id().to_string();
        let locks = std::fs::read_to_string("/proc/locks").unwrap();
        locks.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() >= 8
                && fields[1] == "POSIX"
                && fields[4] == pid
                && fields[5].rsplit(':').next() == Some(inode.as_str())
                && fields[6].parse::<u64>().is_ok_and(|start| start <= 128)
                && (fields[7] == "EOF" || fields[7].parse::<u64>().is_ok_and(|end| end >= 128))
        })
    };
    let plain = rusqlite::Connection::open(dir.path().join("plain.db")).unwrap();
    plain
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE example(value);")
        .unwrap();
    assert!(
        holds_deadman_lock("plain.db-shm"),
        "positive control must retain the WAL deadman lock"
    );

    let repository = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    repository.check_writable().unwrap();
    // An unrelated close() must not release SQLite's cross-process protection.
    assert!(
        holds_deadman_lock("policy-state.db-shm"),
        "Repository file validation released SQLite's WAL deadman lock"
    );
    let initial = seed(&repository);
    let applying = transition(&repository, &initial, BindingStatus::Applying);
    assert_eq!(
        repository
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap(),
        Some(applying)
    );
    assert!(
        holds_deadman_lock("policy-state.db-shm"),
        "Repository reads and writes must preserve SQLite's WAL deadman lock"
    );
}

trait Repository: PapRepository + BindingStateRepository + BindingReconcileCatalog {}
impl<T: PapRepository + BindingStateRepository + BindingReconcileCatalog> Repository for T {}

fn each(test: impl Fn(&dyn Repository)) {
    test(&ProcessLocalPapRepository::default());
    let dir = private_dir();
    test(&SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap());
}

fn seed(repo: &dyn Repository) -> BindingStateSnapshot {
    let binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repo.put_policy(&binding.policy).unwrap();
    repo.put_scope(&PreparedScope {
        scope_id: binding.scope.scope_id.clone(),
        selector: binding.scope.selector.clone(),
        policy_snapshots: vec![binding.policy],
        status: ScopeStatus::Active,
    })
    .unwrap();
    let intent = repo
        .sync_scope_instances(&binding.scope.scope_id, &[binding.scope.process])
        .unwrap()
        .remove(0);
    assert_eq!(intent.status_version, 1);
    repo.get_binding_state(&intent.spec.binding_id)
        .unwrap()
        .unwrap()
}
fn transition(
    repo: &dyn Repository,
    current: &BindingStateSnapshot,
    phase: BindingStatus,
) -> BindingStateSnapshot {
    let mut next = current.clone();
    next.binding.status = phase.into();
    let result = repo
        .compare_exchange_binding_state(current, &BindingStateWrite::new(next))
        .unwrap();
    let WriteResult::Applied(receipt) = result else {
        panic!("unexpected result {result:?}");
    };
    let next = repo
        .get_binding_state(&current.binding.spec.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status_version, Some(next.status_version));
    assert!(receipt.status_applied);
    next
}
fn target() -> TargetRef {
    TargetRef {
        route: "test".into(),
        id: "target".into(),
        cleanup: vec![1, 2, 3],
    }
}

#[test]
fn status_version_fences_aba_and_late_enqueue_failure() {
    each(|repo| {
        let initial = seed(repo);
        let stale = BindingIntentReceipt::from(&initial);
        assert!(
            repo.fail_pending_binding(&stale, asc_pap::EnqueueError::Full)
                .unwrap()
        );
        let receipt = repo
            .retry_scope(&initial.binding.spec.scope.scope_id)
            .unwrap()
            .remove(0);
        assert_eq!(receipt.status_version, 3);
        assert_eq!(receipt.binding, initial.binding);
        assert!(
            !repo
                .fail_pending_binding(&stale, asc_pap::EnqueueError::Full)
                .unwrap()
        );
        let mut next = initial.clone();
        next.binding.status = BindingStatus::Applying.into();
        assert_eq!(
            repo.compare_exchange_binding_state(&initial, &BindingStateWrite::new(next))
                .unwrap(),
            WriteResult::Conflict
        );
        assert_eq!(
            repo.get_binding_state(&initial.binding.spec.binding_id)
                .unwrap()
                .unwrap()
                .status_version,
            3
        );
    });
}

#[test]
fn old_apply_observation_survives_new_delete_without_overwriting_it() {
    each(|repo| {
        let initial = seed(repo);
        let applying = transition(repo, &initial, BindingStatus::Applying);
        let registration = BindingStateWrite::patch(ReconciliationPatch {
            deployments: Some(vec![Deployment {
                target: target(),
                revision: applying.binding.spec.binding_revision,
                presence: Presence::Unknown,
                last_confirmed: None,
            }]),
            ..Default::default()
        });
        repo.compare_exchange_binding_state(&applying, &registration)
            .unwrap();
        let registered = repo
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(registered.status_version, 2);
        repo.begin_scope_delete(&initial.binding.spec.scope.scope_id)
            .unwrap();
        repo.finish_scope_discovery(&initial.binding.spec.scope.scope_id)
            .unwrap();
        let mut deployments = registered.deployments.clone();
        deployments[0].presence = Presence::Present;
        deployments[0].last_confirmed = Some(Presence::Present);
        let write = BindingStateWrite::patch(ReconciliationPatch {
            status: Some(BindingStatus::Ready.into()),
            deployments: Some(deployments),
            preserve_observations_on_conflict: true,
        });
        assert_eq!(
            repo.compare_exchange_binding_state(&registered, &write)
                .unwrap(),
            WriteResult::Applied(WriteReceipt {
                status_version: Some(3),
                status_applied: false
            })
        );
        assert_eq!(
            repo.compare_exchange_binding_state(&registered, &write)
                .unwrap(),
            WriteResult::AlreadyApplied(WriteReceipt {
                status_version: Some(3),
                status_applied: false
            })
        );
        let current = repo
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.binding.status, BindingStatus::PendingDelete);
        assert_eq!(current.deployments[0].presence, Presence::Present);
    });
}

#[test]
fn no_op_and_deployment_only_writes_do_not_bump_status() {
    each(|repo| {
        let initial = seed(repo);
        let same = transition(repo, &initial, BindingStatus::PendingApply);
        assert_eq!(same.status_version, 1);
        let running = transition(repo, &same, BindingStatus::Applying);
        assert_eq!(running.status_version, 2);
        let write = BindingStateWrite::patch(ReconciliationPatch {
            deployments: Some(Vec::new()),
            ..Default::default()
        });
        assert_eq!(
            repo.compare_exchange_binding_state(&running, &write)
                .unwrap(),
            WriteResult::Applied(WriteReceipt {
                status_version: Some(2),
                status_applied: true
            })
        );
        let mut collision = write.clone();
        collision.next.as_mut().unwrap().status = Some(BindingStatus::Ready.into());
        assert_eq!(
            repo.compare_exchange_binding_state(&running, &collision),
            Err(StoreError::Invalid)
        );
    });
}

#[test]
fn scope_and_last_binding_disappear_together_but_active_scope_survives() {
    each(|repo| {
        let initial = seed(repo);
        let scope = &initial.binding.spec.scope.scope_id;
        let retired = repo.sync_scope_instances(scope, &[]).unwrap();
        assert_eq!(retired.len(), 1);
        let pending = repo
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap()
            .unwrap();
        let deleting = transition(repo, &pending, BindingStatus::Deleting);
        repo.compare_exchange_binding_state(&deleting, &BindingStateWrite::delete())
            .unwrap();
        assert_eq!(repo.get_scope(scope).unwrap().status, ScopeStatus::Active);
        assert!(
            repo.scope_discovery_seed(scope)
                .unwrap()
                .pinned_process
                .is_some()
        );
        repo.begin_scope_delete(scope).unwrap();
        repo.finish_scope_discovery(scope).unwrap();
        assert_eq!(repo.get_scope(scope), Err(PapError::NotFound));
    });
}

#[test]
fn deleting_scope_waits_for_last_binding_and_discovery_barrier() {
    each(|repo| {
        let initial = seed(repo);
        let scope = &initial.binding.spec.scope.scope_id;
        repo.begin_scope_delete(scope).unwrap();
        repo.finish_scope_discovery(scope).unwrap();
        assert_eq!(repo.get_scope(scope).unwrap().status, ScopeStatus::Deleting);
        let pending = repo
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap()
            .unwrap();
        let deleting = transition(repo, &pending, BindingStatus::Deleting);
        repo.compare_exchange_binding_state(&deleting, &BindingStateWrite::delete())
            .unwrap();
        assert!(
            repo.get_binding_state(&initial.binding.spec.binding_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(repo.get_scope(scope), Err(PapError::NotFound));
    });
}

#[test]
fn reopen_preserves_snapshots_versions_receipts_and_pid_pin() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let initial = seed(&repo);
    let mut next = initial.clone();
    next.binding.status = BindingStatus::Applying.into();
    let write = BindingStateWrite::new(next);
    let applied = repo
        .compare_exchange_binding_state(&initial, &write)
        .unwrap();
    assert!(matches!(applied, WriteResult::Applied(_)));
    repo.delete_policy_revision(
        &initial.binding.spec.policy.policy_id,
        initial.binding.spec.policy.revision,
    )
    .unwrap();
    let before = repo
        .get_binding_state(&initial.binding.spec.binding_id)
        .unwrap();
    drop(repo);
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(
        repo.get_binding_state(&initial.binding.spec.binding_id)
            .unwrap(),
        before
    );
    assert!(matches!(
        repo.compare_exchange_binding_state(&initial, &write)
            .unwrap(),
        WriteResult::AlreadyApplied(WriteReceipt {
            status_version: Some(2),
            status_applied: true
        })
    ));
    let seed = repo
        .scope_discovery_seed(&initial.binding.spec.scope.scope_id)
        .unwrap();
    assert_eq!(
        seed.pinned_process,
        Some(initial.binding.spec.scope.process.clone())
    );
    assert_eq!(
        seed.scope.policy_snapshots,
        std::slice::from_ref(&initial.binding.spec.policy)
    );
    let head = repo
        .get_policy_revision_state(&initial.binding.spec.policy.policy_id)
        .unwrap()
        .unwrap();
    assert_eq!(head.last_allocated_revision, Revision::new(1).unwrap());
    assert!(head.current.is_none());
}

#[test]
fn strict_open_rejects_unknown_schema_and_preserves_files() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    drop(SqlitePolicyRepository::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(SqlitePolicyRepository::open(&path).is_err());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        99
    );
}

#[test]
fn database_lease_excludes_second_owner_and_unsafe_alias() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert!(SqlitePolicyRepository::open(&path).is_err());
    drop(repo);
    let alias = private_dir();
    std::fs::hard_link(&path, alias.path().join("policy-state.db")).unwrap();
    assert!(SqlitePolicyRepository::open(&path).is_err());
}

#[test]
fn keyset_recovery_does_not_skip_rows_deleted_on_previous_page() {
    let dir = private_dir();
    let repo = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    let initial = seed(&repo);
    let base = repo
        .get_scope(&initial.binding.spec.scope.scope_id)
        .unwrap();
    for n in 0..8 {
        let mut scope = base.clone();
        scope.scope_id = ResourceId::new(format!("scope-{n}")).unwrap();
        repo.put_scope(&scope).unwrap();
        repo.begin_scope_delete(&scope.scope_id).unwrap();
    }
    let mut after = None;
    let mut removed = 0;
    loop {
        let scopes = repo.scan_scopes(after.as_ref(), 3).unwrap();
        if scopes.is_empty() {
            break;
        }
        for scope in &scopes {
            if scope.status == ScopeStatus::Deleting {
                repo.finish_scope_discovery(&scope.scope_id).unwrap();
                removed += 1;
            }
        }
        after = scopes.last().map(|s| s.scope_id.clone());
    }
    assert_eq!(removed, 8);
    assert_eq!(repo.list_scopes(10, 0).unwrap().total, 1);
}

fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[test]
fn scope_capacity_and_snapshot_admission_are_atomic() {
    each(|repo| {
        let initial = seed(repo);
        let template = repo
            .get_scope(&initial.binding.spec.scope.scope_id)
            .unwrap();
        for index in 1..32 {
            let mut scope = template.clone();
            scope.scope_id = ResourceId::new(format!("scope-{index:02}")).unwrap();
            repo.put_scope(&scope).unwrap();
        }
        let mut rejected = template.clone();
        rejected.scope_id = ResourceId::new("overflow").unwrap();
        assert_eq!(repo.put_scope(&rejected), Err(PapError::Unavailable));
        assert_eq!(repo.list_scopes(100, 0).unwrap().total, 32);
        repo.delete_policy_revision(
            &initial.binding.spec.policy.policy_id,
            initial.binding.spec.policy.revision,
        )
        .unwrap();
        assert_eq!(
            repo.put_scope(&rejected),
            Err(PapError::ReferencedPolicyRevisionNotFound)
        );
        assert_eq!(repo.get_scope(&template.scope_id).unwrap(), template);
    });
}

#[test]
fn invalid_authoritative_rows_are_rejected_without_repair() {
    for sql in [
        "PRAGMA ignore_check_constraints=ON; UPDATE bindings SET spec_json='broken-json'",
        "UPDATE bindings SET policy_id='wrong-projection'",
        "PRAGMA foreign_keys=OFF; UPDATE bindings SET scope_id='missing-owner'",
        "UPDATE bindings SET last_write_id='not-a-uuid',last_write_digest=zeroblob(32),last_write_result_json='{}'",
    ] {
        let dir = private_dir();
        let path = dir.path().join("policy-state.db");
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        seed(&repo);
        drop(repo);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(sql).unwrap();
        drop(db);
        assert!(matches!(
            SqlitePolicyRepository::open(&path),
            Err(asc_policy_repository_sqlite::RepositoryError::Storage(
                StoreError::Corrupt
            ))
        ));
        assert!(path.exists());
    }
}

#[test]
fn version_overflow_rolls_back_scope_barrier_and_every_child() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let initial = seed(&repo);
    let scope_id = &initial.binding.spec.scope.scope_id;
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute("UPDATE bindings SET status_version=?1", [i64::MAX])
        .unwrap();
    repo.begin_scope_delete(scope_id).unwrap();
    assert_eq!(
        repo.finish_scope_discovery(scope_id),
        Err(PapError::Persistence)
    );
    let (phase, stopped): (String, i64) = sql
        .query_row("SELECT phase,discovery_stopped FROM scopes", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((phase.as_str(), stopped), ("DELETING", 0));
    let saved = repo
        .get_binding_state(&initial.binding.spec.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.status_version, i64::MAX);
    assert_eq!(saved.binding.status.phase, BindingStatus::PendingApply);
}

#[test]
fn busy_recovers_without_losing_state_and_lease_outlives_repository() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let initial = seed(&repo);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(repo.check_writable(), Err(StoreError::Busy));
    assert_eq!(
        repo.begin_scope_delete(&initial.binding.spec.scope.scope_id),
        Err(PapError::Unavailable)
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(repo.check_writable(), Ok(()));
    assert_eq!(
        repo.get_binding_state(&initial.binding.spec.binding_id)
            .unwrap(),
        Some(initial)
    );
    let lease = repo.lease();
    drop(repo);
    assert!(matches!(
        SqlitePolicyRepository::open(&path),
        Err(asc_policy_repository_sqlite::RepositoryError::AlreadyOpen)
    ));
    drop(lease);
    SqlitePolicyRepository::open(&path).unwrap();
}

#[test]
fn unsafe_permissions_and_symlink_ancestors_are_rejected() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(SqlitePolicyRepository::open(&path).is_err());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    for name in [
        "policy-state.db",
        "policy-state.lock",
        "policy-state.db-wal",
        "policy-state.db-shm",
    ] {
        assert_eq!(
            std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(repo);
    let alias_dir = private_dir();
    std::os::unix::fs::symlink(dir.path(), alias_dir.path().join("alias")).unwrap();
    assert!(SqlitePolicyRepository::open(&alias_dir.path().join("alias/policy-state.db")).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(SqlitePolicyRepository::open(&path).is_err());
}

#[test]
fn metadata_checks_reject_unsafe_database_and_sidecars() {
    use asc_policy_repository_sqlite::DatabaseLease;

    for name in [
        "policy-state.db",
        "policy-state.db-wal",
        "policy-state.db-shm",
    ] {
        for invalid in ["permissions", "symlink", "hardlink"] {
            let dir = private_dir();
            let database = dir.path().join("policy-state.db");
            drop(DatabaseLease::acquire(&database).unwrap());
            let path = dir.path().join(name);
            std::fs::write(&path, []).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let alias = dir.path().join("alias");
            match invalid {
                "permissions" => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                "symlink" => {
                    std::fs::rename(&path, &alias).unwrap();
                    std::os::unix::fs::symlink(&alias, &path).unwrap();
                }
                "hardlink" => std::fs::hard_link(&path, &alias).unwrap(),
                _ => unreachable!(),
            }
            assert!(
                DatabaseLease::acquire(&database).is_err(),
                "accepted unsafe {name}: {invalid}"
            );
        }
    }
}

#[test]
fn scope_admission_serializes_with_template_update_and_delete() {
    use std::sync::{Arc, Barrier};
    for delete in [false, true] {
        each(|repo| {
            let initial = seed(repo);
            let mut scope = repo
                .get_scope(&initial.binding.spec.scope.scope_id)
                .unwrap();
            scope.scope_id = ResourceId::new("concurrent-assignment").unwrap();
            let mut policy = initial.binding.spec.policy.clone();
            policy.revision = Revision::new(2).unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let created = std::thread::scope(|threads| {
                let create = threads.spawn(|| {
                    barrier.wait();
                    repo.put_scope(&scope)
                });
                barrier.wait();
                if delete {
                    repo.delete_policy_revision(&policy.policy_id, Revision::new(1).unwrap())
                        .unwrap();
                } else {
                    repo.put_policy(&policy).unwrap();
                }
                create.join().unwrap()
            });
            match created {
                Ok(saved) => {
                    assert_eq!(saved, scope);
                    assert_eq!(repo.get_scope(&scope.scope_id).unwrap(), scope);
                    assert_eq!(saved.policy_snapshots[0].revision.get(), 1);
                }
                Err(PapError::ReferencedPolicyRevisionNotFound) => {
                    assert_eq!(repo.get_scope(&scope.scope_id), Err(PapError::NotFound));
                }
                other => panic!("unexpected admission result: {other:?}"),
            }
        });
    }
}

#[test]
fn discovery_is_idempotent_and_pages_use_global_byte_order() {
    each(|repo| {
        let initial = seed(repo);
        let scope = &initial.binding.spec.scope.scope_id;
        let process = &initial.binding.spec.scope.process;
        for _ in 0..3 {
            repo.sync_scope_instances(scope, std::slice::from_ref(process))
                .unwrap();
        }
        assert_eq!(repo.list_bindings(10, 0).unwrap().total, 1);
        let mut names = vec![initial.binding.spec.policy.policy_id.to_string()];
        for key in ["z", "Z", "a", "A", "a-1"] {
            let mut policy = initial.binding.spec.policy.clone();
            policy.policy_id = ResourceId::new(key).unwrap();
            repo.put_policy(&policy).unwrap();
            names.push(key.into());
        }
        names.sort();
        let mut listed = Vec::new();
        loop {
            let page = repo
                .list_policies(2, u32::try_from(listed.len()).unwrap())
                .unwrap();
            assert_eq!(usize::try_from(page.total).unwrap(), names.len());
            if page.items.is_empty() {
                break;
            }
            listed.extend(page.items.into_iter().map(|p| p.policy_id.to_string()));
        }
        assert_eq!(listed, names);
    });
}

#[test]
fn oversized_deployment_registration_is_rejected_atomically() {
    each(|repo| {
        let initial = seed(repo);
        let mut deployment = Deployment {
            target: target(),
            revision: initial.binding.spec.binding_revision,
            presence: Presence::Unknown,
            last_confirmed: None,
        };
        let too_many = (0..33)
            .map(|index| {
                let mut item = deployment.clone();
                item.target.id = format!("target-{index}");
                item
            })
            .collect();
        deployment.target.cleanup = vec![0; 1024 * 1024];
        for deployments in [too_many, vec![deployment]] {
            let write = BindingStateWrite::patch(ReconciliationPatch {
                deployments: Some(deployments),
                ..Default::default()
            });
            assert_eq!(
                repo.compare_exchange_binding_state(&initial, &write),
                Err(StoreError::Invalid)
            );
            assert_eq!(
                repo.get_binding_state(&initial.binding.spec.binding_id)
                    .unwrap(),
                Some(initial.clone())
            );
        }
    });
}

#[test]
fn closed_database_backup_preserves_delete_responsibility_and_receipt() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let initial = seed(&repo);
    let write = BindingStateWrite::patch(ReconciliationPatch {
        deployments: Some(vec![Deployment {
            target: target(),
            revision: initial.binding.spec.binding_revision,
            presence: Presence::Unknown,
            last_confirmed: None,
        }]),
        ..Default::default()
    });
    repo.compare_exchange_binding_state(&initial, &write)
        .unwrap();
    repo.begin_scope_delete(&initial.binding.spec.scope.scope_id)
        .unwrap();
    repo.finish_scope_discovery(&initial.binding.spec.scope.scope_id)
        .unwrap();
    let expected = repo
        .get_binding_state(&initial.binding.spec.binding_id)
        .unwrap();
    drop(repo);
    let backup = private_dir();
    let restored = backup.path().join("policy-state.db");
    // Closing the last connection checkpoints WAL; copying a live DB alone is unsafe.
    std::fs::copy(&path, &restored).unwrap();
    let repo = SqlitePolicyRepository::open(&restored).unwrap();
    assert_eq!(
        repo.get_binding_state(&initial.binding.spec.binding_id)
            .unwrap(),
        expected
    );
    assert!(matches!(
        repo.compare_exchange_binding_state(&initial, &write)
            .unwrap(),
        WriteResult::AlreadyApplied(_)
    ));
    assert_eq!(
        repo.get_scope(&initial.binding.spec.scope.scope_id)
            .unwrap()
            .status,
        ScopeStatus::Deleting
    );
}

#[test]
fn batch_discovery_and_cleanup_keep_all_children_atomic() {
    use asc_policy_types::scope::{ProcessMatcher, ScopeSelector};
    use std::time::Instant;
    let dir = private_dir();
    let repo = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    let initial = seed(&repo);
    let mut scope = repo
        .get_scope(&initial.binding.spec.scope.scope_id)
        .unwrap();
    scope.scope_id = ResourceId::new("batch").unwrap();
    scope.selector = ScopeSelector::Process {
        matcher: ProcessMatcher::Name {
            process_name: "worker".into(),
        },
    };
    repo.put_scope(&scope).unwrap();
    let instances: Vec<_> = (10_000..10_200)
        .map(|pid| {
            let mut instance = initial.binding.spec.scope.process.clone();
            instance.pid = pid;
            instance
        })
        .collect();
    let started = Instant::now();
    let intents = repo
        .sync_scope_instances(&scope.scope_id, &instances)
        .unwrap();
    let admission = started.elapsed();
    assert_eq!(intents.len(), 200);
    repo.begin_scope_delete(&scope.scope_id).unwrap();
    let started = Instant::now();
    let deleting = repo.finish_scope_discovery(&scope.scope_id).unwrap();
    let cleanup_intent = started.elapsed();
    assert_eq!(deleting.len(), 200);
    assert!(
        deleting
            .iter()
            .all(|b| b.status_version == 2 && b.status == BindingStatus::PendingDelete)
    );
    let started = Instant::now();
    for intent in deleting {
        let current = repo
            .get_binding_state(&intent.spec.binding_id)
            .unwrap()
            .unwrap();
        let deleting = transition(&repo, &current, BindingStatus::Deleting);
        repo.compare_exchange_binding_state(&deleting, &BindingStateWrite::delete())
            .unwrap();
    }
    let finalize = started.elapsed();
    assert_eq!(repo.get_scope(&scope.scope_id), Err(PapError::NotFound));
    assert_eq!(repo.list_bindings(10, 0).unwrap().total, 1);
    eprintln!(
        "200 bindings, WAL/FULL: admission={admission:?}, delete_intent={cleanup_intent:?}, finalization={finalize:?}"
    );
    for file in [
        "policy-state.db",
        "policy-state.db-wal",
        "policy-state.db-shm",
    ] {
        eprintln!(
            "{file}: {} bytes",
            std::fs::metadata(dir.path().join(file)).unwrap().len()
        );
    }
}

#[test]
fn general_policy_rules_survive_reopen_in_current_scope_and_binding_snapshots() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let mut policy = binding.policy;
    policy.template = serde_json::from_value(serde_json::json!({
        "specVersion":"0.1", "description":"Reusable policy requiring review",
        "rules":[{"effect":"require_confirmation","category":"file","action":"exec",
            "target":{"type":"file","path":"/usr/bin/git"},
            "where":{"and":[{"argsPrefix":{"eq":["push"]}},{"cwd":{"eq":"/workspace"}}]},
            "previous":{"category":"file","action":"read","target":{"type":"file","path":"/secrets/**"}},
            "because":"Review before pushing"}]
    })).unwrap();
    repo.put_policy(&policy).unwrap();
    let mut scopes = Vec::new();
    let mut bindings = Vec::new();
    for id in ["assignment-one", "assignment-two"] {
        let scope = PreparedScope {
            scope_id: ResourceId::new(id).unwrap(),
            selector: binding.scope.selector.clone(),
            policy_snapshots: vec![policy.clone()],
            status: ScopeStatus::Active,
        };
        repo.put_scope(&scope).unwrap();
        bindings.extend(
            repo.sync_scope_instances(
                &scope.scope_id,
                std::slice::from_ref(&binding.scope.process),
            )
            .unwrap(),
        );
        scopes.push(scope);
    }
    drop(repo);
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(
        repo.get_policy(&policy.policy_id, policy.revision).unwrap(),
        policy
    );
    for scope in &scopes {
        assert_eq!(repo.get_scope(&scope.scope_id).unwrap(), *scope);
    }
    for receipt in &bindings {
        let restored = repo
            .get_binding_state(&receipt.spec.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(restored.binding.spec.policy, policy);
        assert_eq!(restored.binding.spec, receipt.spec);
    }
    repo.delete_policy_revision(&policy.policy_id, policy.revision)
        .unwrap();
    drop(repo);
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    for scope in &scopes {
        assert_eq!(repo.get_scope(&scope.scope_id).unwrap(), *scope);
    }
    for receipt in &bindings {
        assert_eq!(
            repo.get_binding_state(&receipt.spec.binding_id)
                .unwrap()
                .unwrap()
                .binding
                .spec
                .policy,
            policy
        );
    }
}

#[test]
fn legacy_policy_payloads_are_rejected_without_discarding_saved_responsibility() {
    for (table, column, json_path) in [
        ("policies", "current_json", "$.template"),
        ("scopes", "assignment_json", "$.policySnapshots[0].template"),
        ("bindings", "spec_json", "$.policy.template"),
    ] {
        let dir = private_dir();
        let path = dir.path().join("policy-state.db");
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        let initial = seed(&repo);
        let mut registered = initial.clone();
        registered.deployments.push(Deployment {
            target: target(),
            revision: initial.binding.spec.binding_revision,
            presence: Presence::Unknown,
            last_confirmed: None,
        });
        repo.compare_exchange_binding_state(&initial, &BindingStateWrite::new(registered))
            .unwrap();
        if table != "policies" {
            repo.delete_policy_revision(
                &initial.binding.spec.policy.policy_id,
                initial.binding.spec.policy.revision,
            )
            .unwrap();
        }
        drop(repo);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            &format!("UPDATE {table} SET {column}=json_set({column},?1,json(?2))"),
            rusqlite::params![
                json_path,
                r#"{"kind":"prevent_file_deletion","files":["/protected"]}"#
            ],
        )
        .unwrap();
        let read = |db: &rusqlite::Connection| -> (String, String) {
            (
                db.query_row(&format!("SELECT {column} FROM {table}"), [], |r| r.get(0))
                    .unwrap(),
                db.query_row("SELECT deployments_json FROM bindings", [], |r| r.get(0))
                    .unwrap(),
            )
        };
        let before = read(&db);
        drop(db);
        assert!(
            SqlitePolicyRepository::open(&path).is_err(),
            "{table} legacy payload must not be silently accepted"
        );
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(read(&db), before);
    }
}
