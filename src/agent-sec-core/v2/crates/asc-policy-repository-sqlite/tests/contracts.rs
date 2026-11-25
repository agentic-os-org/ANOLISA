use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{PapError, PapRepository, PolicySnapshotKind};
use asc_policy_repository::*;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::{
    binding::{BindingStatus, PreparedBinding},
    scope::{PreparedScope, ScopeStatus},
    target::{Presence, TargetRef},
};
use std::os::unix::fs::PermissionsExt;

mod common;
use common::{Repository, private_dir, seed, transition};

fn each(test: impl Fn(&dyn Repository)) {
    let dir = private_dir();
    test(&SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap());
}

fn target() -> TargetRef {
    TargetRef {
        route: "test".into(),
        id: "target".into(),
        cleanup: vec![1, 2, 3],
    }
}

#[test]
fn scope_delete_intent_fences_apply_before_discovery_cleanup() {
    each(|repo| {
        let initial = seed(repo);
        let scope_id = &initial.binding.spec.scope.scope_id;
        repo.begin_scope_delete(scope_id).unwrap();
        let mut applying = initial.clone();
        applying.binding.status = BindingStatus::Applying.into();
        assert_eq!(
            repo.compare_exchange_binding_state(&initial, &BindingStateWrite::new(applying))
                .unwrap(),
            WriteResult::Conflict
        );
        assert_eq!(
            repo.get_binding_state(&initial.binding.spec.binding_id)
                .unwrap(),
            Some(initial.clone())
        );
        repo.finish_scope_discovery(scope_id).unwrap();
        let deleting = repo
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(deleting.binding.status.phase, BindingStatus::PendingDelete);
        transition(repo, &deleting, BindingStatus::Deleting);
    });
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
fn new_delete_rejects_the_entire_old_apply_result() {
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
        });
        assert_eq!(
            repo.compare_exchange_binding_state(&registered, &write)
                .unwrap(),
            WriteResult::Conflict
        );
        assert_eq!(
            repo.compare_exchange_binding_state(&registered, &write)
                .unwrap(),
            WriteResult::Conflict
        );
        let current = repo
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.binding.status, BindingStatus::PendingDelete);
        assert_eq!(current.status_version, 3);
        assert_eq!(current.deployments, registered.deployments);
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
fn credential_scope_release_responsibility_survives_restart_with_zero_or_last_binding() {
    use asc_policy_types::scope::{ProcessMatcher, ScopeSelector};
    for with_binding in [false, true] {
        let dir = private_dir();
        let path = dir.path().join("policy-state.db");
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        let fixture: PreparedBinding = serde_json::from_str(include_str!(
            "../../asc-policy-types/tests/fixtures/prepared-binding.json"
        ))
        .unwrap();
        let mut policy = fixture.policy;
        let source = asc_policy_types::cedar::CedarPolicySource {
            schema: asc_policy_types::cedar::CedarSchemaReference {
                schema_id: ResourceId::new("seccore.system").unwrap(),
                revision: Revision::new(2).unwrap(),
            },
            text: r#"@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when { resource.host == "api.github.com" && context.input.method == "GET" && context.input.path like "/repos/*" };"#.into(),
        };
        policy.credential_bindings = asc_policy_cedar::http::compile(&source)
            .unwrap()
            .into_iter()
            .map(|rule| asc_policy_types::policy::CredentialBinding {
                rule_id: rule.rule_id,
                pair_id: "pair-one".into(),
            })
            .collect();
        policy.template = asc_policy_types::authoring::PolicySource::Cedar(source);
        for binding in [None, Some("unrelated-rule")] {
            let mut invalid = policy.clone();
            if let Some(rule_id) = binding {
                invalid.credential_bindings[0].rule_id = rule_id.into();
            } else {
                invalid.credential_bindings.clear();
            }
            assert!(matches!(
                repo.put_policy(&invalid),
                Err(PapError::InvalidCedar(_))
            ));
            assert_eq!(
                repo.get_policy(&policy.policy_id, policy.revision),
                Err(PapError::NotFound)
            );
        }
        repo.put_policy(&policy).unwrap();
        let scope = PreparedScope {
            scope_id: fixture.scope.scope_id,
            selector: ScopeSelector::Process {
                matcher: ProcessMatcher::Name {
                    process_name: "agent".into(),
                },
            },
            owner_uid: Some(1000),
            policy_snapshots: vec![policy.clone()],
            status: ScopeStatus::Active,
        };
        repo.put_scope(&scope).unwrap();
        assert_eq!(
            repo.finish_scope_execution(&scope.scope_id),
            Err(PapError::Conflict)
        );
        if with_binding {
            repo.sync_scope_instances(&scope.scope_id, &[fixture.scope.process])
                .unwrap();
        }
        repo.delete_policy_revision(&policy.policy_id, policy.revision)
            .unwrap();
        repo.begin_scope_delete(&scope.scope_id).unwrap();
        repo.finish_scope_discovery(&scope.scope_id).unwrap();
        assert_eq!(
            repo.scope_execution_ready_to_release(&scope.scope_id)
                .unwrap(),
            !with_binding
        );
        if with_binding {
            assert_eq!(
                repo.finish_scope_execution(&scope.scope_id),
                Err(PapError::Conflict)
            );
            let intent = repo.list_bindings(10, 0).unwrap().items.remove(0);
            let pending = repo
                .get_binding_state(&intent.spec.binding_id)
                .unwrap()
                .unwrap();
            let deleting = transition(&repo, &pending, BindingStatus::Deleting);
            repo.compare_exchange_binding_state(&deleting, &BindingStateWrite::delete())
                .unwrap();
        }
        let mut expected = scope;
        expected.status = ScopeStatus::Deleting;
        assert_eq!(repo.get_scope(&expected.scope_id).unwrap(), expected);
        drop(repo);
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        assert_eq!(repo.get_scope(&expected.scope_id).unwrap(), expected);
        assert!(
            repo.scope_execution_ready_to_release(&expected.scope_id)
                .unwrap()
        );
        // Discovery completion alone never discards credential release responsibility.
        repo.finish_scope_discovery(&expected.scope_id).unwrap();
        assert_eq!(repo.get_scope(&expected.scope_id).unwrap(), expected);
        repo.finish_scope_execution(&expected.scope_id).unwrap();
        repo.finish_scope_execution(&expected.scope_id).unwrap();
        assert_eq!(repo.get_scope(&expected.scope_id), Err(PapError::NotFound));
        drop(repo);
        let repo = SqlitePolicyRepository::open(&path).unwrap();
        assert_eq!(repo.list_scopes(10, 0).unwrap().total, 0);
    }
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

fn restore_v1_schema(db: &rusqlite::Connection) {
    db.execute_batch("DROP TABLE policy_generation; PRAGMA user_version=1")
        .unwrap();
}

fn assert_v1_schema(db: &rusqlite::Connection) {
    let state: (i64, bool) = db
        .query_row(
            "SELECT user_version, EXISTS(SELECT 1 FROM sqlite_schema WHERE name='policy_generation')
             FROM pragma_user_version",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (1, false));
}

#[test]
fn v1_schema_mismatch_rolls_back_until_the_database_is_repaired() {
    use asc_policy_repository_sqlite::RepositoryError;

    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let saved = seed(&repo);
    drop(repo);
    let db = rusqlite::Connection::open(&path).unwrap();
    restore_v1_schema(&db);
    db.execute_batch("CREATE TABLE extra AS SELECT 'kept' AS value")
        .unwrap();
    assert!(matches!(
        SqlitePolicyRepository::open(&path),
        Err(RepositoryError::Storage(StoreError::Incompatible))
    ));
    assert_v1_schema(&db);
    let value: String = db
        .query_row("SELECT value FROM extra", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "kept");
    db.execute("DROP TABLE extra", []).unwrap();
    drop(db);

    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(repo.policy_generation().unwrap(), 0);
    assert_eq!(
        repo.get_binding_state(&saved.binding.spec.binding_id)
            .unwrap(),
        Some(saved)
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
fn sql_list_byte_budget_preserves_total_and_advances_every_page() {
    let dir = private_dir();
    let repo = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    let initial = seed(&repo).binding.spec.policy;
    let mut policy = initial.clone();
    let mut template = serde_json::to_value(&policy).unwrap()["template"].clone();
    let rule = template["rules"][0].clone();
    template["rules"] = serde_json::Value::Array(
        (0..200)
            .map(|index| {
                let mut rule = rule.clone();
                rule["target"]["path"] = format!("/{index}{}", "x".repeat(3000)).into();
                rule
            })
            .collect(),
    );
    policy.template = asc_policy_types::authoring::PolicySource::SecCore(
        serde_json::from_value(template).unwrap(),
    );
    let mut expected = vec![initial];
    for index in 0..7 {
        policy.policy_id = ResourceId::new(format!("large-{index}")).unwrap();
        expected.push(repo.put_policy(&policy).unwrap());
    }
    expected.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
    let mut actual = vec![];
    while actual.len() < expected.len() {
        let page = repo
            .list_policies(100, u32::try_from(actual.len()).unwrap())
            .unwrap();
        assert_eq!(page.total, u64::try_from(expected.len()).unwrap());
        assert!(
            !page.items.is_empty(),
            "byte-limited pagination must make progress"
        );
        assert!(serde_json::to_vec(&page.items).unwrap().len() <= 3 * 1024 * 1024 + 1);
        if actual.is_empty() {
            assert!(
                page.items.len() < expected.len(),
                "SQL page must hit the byte budget"
            );
        }
        actual.extend(page.items);
    }
    assert_eq!(actual, expected);
    let end = repo
        .list_policies(100, u32::try_from(actual.len()).unwrap())
        .unwrap();
    assert_eq!(end.total, u64::try_from(expected.len()).unwrap());
    assert!(end.items.is_empty());
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
    policy.template = asc_policy_types::authoring::PolicySource::SecCore(serde_json::from_value(serde_json::json!({
        "specVersion":"0.1", "description":"Reusable policy requiring review",
        "rules":[{"effect":"require_confirmation","category":"file","action":"exec",
            "target":{"type":"file","path":"/usr/bin/git"},
            "where":{"and":[{"argsPrefix":{"eq":["push"]}},{"cwd":{"eq":"/workspace"}}]},
            "previous":{"category":"file","action":"read","target":{"type":"file","path":"/secrets/**"}},
            "because":"Review before pushing"}]
    })).unwrap());
    repo.put_policy(&policy).unwrap();
    let mut scopes = Vec::new();
    let mut bindings = Vec::new();
    for id in ["assignment-one", "assignment-two"] {
        let scope = PreparedScope {
            scope_id: ResourceId::new(id).unwrap(),
            selector: binding.scope.selector.clone(),
            policy_snapshots: vec![policy.clone()],
            owner_uid: None,
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
fn cedar_snapshots_reload_after_update_delete_and_restart_using_their_saved_schema() {
    use asc_pap::PapService;
    use asc_policy_cedar::pst;
    use asc_policy_types::authoring::PolicySource;
    use std::sync::Arc;

    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = Arc::new(SqlitePolicyRepository::open(&path).unwrap());
    let pap = PapService::new(repo.clone());
    let source: PolicySource = serde_json::from_value(serde_json::json!({
        "language":"cedar", "content":{
            "schema":{"schemaId":"seccore.system","revision":1},
            "text":"// Retain whitespace and original source.\nforbid(principal, action == SecCore::Action::\"file.write\", resource) when { resource.path == \"/protected\" };\n"
        }
    })).unwrap();
    let original = pap.create_policy("cedar", &source).unwrap();
    let fixture: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let scope = PreparedScope {
        scope_id: fixture.scope.scope_id,
        selector: fixture.scope.selector,
        policy_snapshots: vec![original.clone()],
        owner_uid: None,
        status: ScopeStatus::Active,
    };
    repo.put_scope(&scope).unwrap();
    let intent = repo
        .sync_scope_instances(&scope.scope_id, &[fixture.scope.process])
        .unwrap()
        .remove(0);
    let mut updated_source = source.clone();
    let PolicySource::Cedar(cedar) = &mut updated_source else {
        unreachable!()
    };
    cedar.text = cedar.text.replace("/protected", "/changed");
    let current = pap
        .update_policy(&original.policy_id, "renamed", &updated_source)
        .unwrap();
    assert_eq!(current.revision.get(), 2);
    assert_ne!(current.template, original.template);
    assert_eq!(repo.get_scope(&scope.scope_id).unwrap(), scope);
    drop(pap);
    drop(repo);

    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(
        repo.get_policy(&current.policy_id, current.revision)
            .unwrap(),
        current
    );
    assert_eq!(repo.get_scope(&scope.scope_id).unwrap(), scope);
    let restored = repo
        .get_binding_state(&intent.spec.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.binding.spec.policy, original);
    let PolicySource::Cedar(saved) = &restored.binding.spec.policy.template else {
        panic!("saved Cedar policy must produce a PST");
    };
    assert_eq!(saved.schema.revision.get(), 1);
    assert_eq!(pst(saved).unwrap().policies.len(), 1);
    repo.delete_policy_revision(&current.policy_id, current.revision)
        .unwrap();
    drop(repo);

    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        2
    );
    let saved: String = db
        .query_row("SELECT spec_json FROM bindings", [], |r| r.get(0))
        .unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        saved["policy"]["template"],
        serde_json::to_value(&source).unwrap()
    );
    drop(db);
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let restored = repo
        .get_binding_state(&intent.spec.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.binding.spec.policy, original);
    let PolicySource::Cedar(mut invalid) = restored.binding.spec.policy.template else {
        unreachable!()
    };
    assert!(pst(&invalid).is_ok());
    invalid.schema.revision = Revision::new(4).unwrap();
    assert!(matches!(
        pst(&invalid),
        Err(asc_policy_cedar::CedarError::UnsupportedSchema { revision: 4, .. })
    ));
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

#[test]
fn startup_reports_legacy_agent_type_in_current_and_pinned_revisions() {
    use asc_policy_repository_sqlite::RepositoryError;

    let directory = private_dir();
    let path = directory.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let initial = seed(&repo);
    let old = &initial.binding.spec.policy;
    let current_revision = old.revision.checked_next().unwrap();
    drop(repo);

    let legacy = serde_json::json!({
        "language": "cedar",
        "content": {
            "schema": {"schemaId": "seccore.system", "revision": 1},
            "text": r#"forbid(principal, action == SecCore::Action::"file.read", resource)
            when { principal has agent_type && principal.agent_type == "cosh" };"#,
        },
    })
    .to_string();
    let db = rusqlite::Connection::open(&path).unwrap();
    restore_v1_schema(&db);
    db.execute(
        "UPDATE policies SET last_allocated_revision=?2,
         current_json=json_set(current_json, '$.revision', ?2, '$.template', json(?1))",
        rusqlite::params![&legacy, current_revision.get()],
    )
    .unwrap();
    db.execute(
        "UPDATE scopes SET assignment_json=json_set(assignment_json, '$.policySnapshots[0].template', json(?1))",
        [&legacy],
    )
    .unwrap();
    db.execute(
        "UPDATE bindings SET spec_json=json_set(spec_json, '$.policy.template', json(?1))",
        [&legacy],
    )
    .unwrap();
    let saved_sources = || -> (String, String, String) {
        db.query_row(
            "SELECT current_json, assignment_json, spec_json FROM policies CROSS JOIN scopes CROSS JOIN bindings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap()
    };
    let before = saved_sources();
    let Err(error @ RepositoryError::LegacyAgentType(_)) = SqlitePolicyRepository::open(&path)
    else {
        panic!("startup must report all stored revisions that require migration");
    };
    let RepositoryError::LegacyAgentType(revisions) = &error else {
        unreachable!()
    };
    assert_eq!(
        *revisions,
        [
            (old.policy_id.clone(), old.revision),
            (old.policy_id.clone(), current_revision),
        ]
        .into()
    );
    assert!(error.to_string().contains("Migrate to principal.agentType"));
    assert_v1_schema(&db);
    assert_eq!(saved_sources(), before);
}

#[test]
fn selected_current_policies_preserve_cedar_and_legacy_sources_across_reopen() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let (native, cedar) = {
        let repository = std::sync::Arc::new(SqlitePolicyRepository::open(&path).unwrap());
        let native = seed(repository.as_ref()).binding.spec.policy;
        let pap = asc_pap::PapService::new(repository.clone());
        let source: asc_policy_types::authoring::PolicySource =
            serde_json::from_value(serde_json::json!({
                "language": "cedar", "content": {
                    "schema": {"schemaId": "seccore.system", "revision": 1},
                    "text": "permit(principal, action, resource);"
                }
            }))
            .unwrap();
        let cedar = pap.create_policy("sandbox", &source).unwrap();
        assert_eq!(
            pap.current_policies(&[cedar.policy_id.clone(), native.policy_id.clone()])
                .unwrap(),
            vec![cedar.clone(), native.clone()]
        );
        assert!(pap.current_policies(&[]).is_err());
        assert!(
            pap.current_policies(&vec![native.policy_id.clone(); 33])
                .is_err()
        );
        assert_eq!(
            pap.current_policies(&[
                native.policy_id.clone(),
                ResourceId::new("missing").unwrap()
            ]),
            Err(PapError::NotFound)
        );
        (native, cedar)
    };
    let repository = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(
        repository
            .current_policies(&[native.policy_id.clone(), cedar.policy_id.clone()])
            .unwrap(),
        vec![native.clone(), cedar.clone()]
    );
    let native_json = serde_json::to_value(&native).unwrap();
    assert!(native_json["template"].get("specVersion").is_some());
    assert!(native_json["template"].get("language").is_none());
    assert_eq!(
        serde_json::to_value(&cedar).unwrap()["template"]["language"],
        "cedar"
    );
    repository
        .delete_policy_revision(&cedar.policy_id, cedar.revision)
        .unwrap();
    assert_eq!(
        repository.current_policies(&[native.policy_id, cedar.policy_id]),
        Err(PapError::NotFound)
    );
}

fn principal_policy(id: &str) -> asc_policy_types::policy::PreparedPolicy {
    serde_json::from_value(serde_json::json!({
        "policyId":id, "revision":1, "policyName":"source",
        "template":{"language":"cedar","content":{
            "schema":{"schemaId":"seccore.system","revision":3},
            "text":r#"forbid(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource is SecCore::File) when {resource.path == "/secret"};"#
        }}
    })).unwrap()
}

#[test]
fn committed_policy_set_generation_covers_both_snapshot_kinds_and_survives_reopen() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(repo.policy_generation().unwrap(), 0);
    let legacy = seed(&repo).binding.spec.policy;
    assert_eq!(repo.policy_generation().unwrap(), 1);
    let source = principal_policy("qoder");
    repo.put_policy(&source).unwrap();
    repo.put_policy(&source).unwrap();
    let snapshot = repo.policy_snapshot(PolicySnapshotKind::Automatic).unwrap();
    assert_eq!(snapshot.generation, 2);
    assert_eq!(snapshot.policies.as_slice(), std::slice::from_ref(&source));
    let snapshot = repo
        .policy_snapshot(PolicySnapshotKind::AllCurrent)
        .unwrap();
    assert_eq!(snapshot.generation, 2);
    assert_eq!(snapshot.policies.len(), 2);
    assert!(snapshot.policies.contains(&legacy));
    assert!(snapshot.policies.contains(&source));

    let mut next = source.clone();
    next.revision = source.revision.checked_next().unwrap();
    repo.put_policy(&next).unwrap();
    assert_eq!(repo.put_policy(&source), Err(PapError::Conflict));
    assert!(
        repo.delete_policy_revision(&source.policy_id, source.revision)
            .is_err()
    );
    let snapshot = repo.policy_snapshot(PolicySnapshotKind::Automatic).unwrap();
    assert_eq!(snapshot.generation, 3);
    assert_eq!(snapshot.policies.as_slice(), std::slice::from_ref(&next));
    repo.delete_policy_revision(&next.policy_id, next.revision)
        .unwrap();
    drop(repo);

    let repo = SqlitePolicyRepository::open(&path).unwrap();
    assert_eq!(repo.policy_generation().unwrap(), 4);
    let snapshot = repo.policy_snapshot(PolicySnapshotKind::Automatic).unwrap();
    assert_eq!(snapshot.generation, 4);
    assert!(snapshot.policies.is_empty());
}

#[test]
fn generation_write_failure_rolls_back_policy_update_and_deletion() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let source = repo.put_policy(&principal_policy("qoder")).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE policy_generation SET generation=?1", [i64::MAX])
        .unwrap();
    let mut next = source.clone();
    next.revision = source.revision.checked_next().unwrap();
    assert!(repo.put_policy(&next).is_err());
    assert!(
        repo.delete_policy_revision(&source.policy_id, source.revision)
            .is_err()
    );
    let snapshot = repo
        .policy_snapshot(PolicySnapshotKind::AllCurrent)
        .unwrap();
    assert_eq!(snapshot.generation, u64::try_from(i64::MAX).unwrap());
    assert_eq!(snapshot.policies, [source]);
}

// Seed policy state that was valid before aggregate write admission. A single SQL statement
// keeps the fixture independent of the new write gate and avoids parsing thousands of policies.
fn insert_historical_sources(
    path: &std::path::Path,
    policy: &asc_policy_types::policy::PreparedPolicy,
    count: usize,
) {
    rusqlite::Connection::open(path).unwrap().execute(
        "WITH RECURSIVE numbers(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM numbers WHERE i < ?1),
         keys(id) AS (SELECT printf('%s-%04d',?2,i) FROM numbers)
         INSERT INTO policies(policy_id,last_allocated_revision,current_json)
         SELECT id,1,json_set(?3,'$.policyId',id) FROM keys",
        rusqlite::params![count,policy.policy_id.as_str(),serde_json::to_string(policy).unwrap()],
    ).unwrap();
}

#[test]
fn v3_record_limit_is_transactional_and_source_revisions_ignore_legacy_policies() {
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let native: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    drop(SqlitePolicyRepository::open(&path).unwrap());
    insert_historical_sources(&path, &native.policy, 4097);
    insert_historical_sources(&path, &principal_policy("historical"), 4095);
    let repo = std::sync::Arc::new(SqlitePolicyRepository::open(&path).unwrap());
    let results = std::thread::scope(|scope| {
        ["first", "second"]
            .map(|key| {
                scope.spawn({
                    let repo = &repo;
                    move || repo.put_policy(&principal_policy(key))
                })
            })
            .map(|thread| thread.join().unwrap())
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.contains(&Err(PapError::AutomaticPolicyCapacity)));
    let revisions = repo.automatic_source_revisions().unwrap();
    assert_eq!(revisions.len(), 4096);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert!(revisions.contains(&(winner.policy_id.clone(), winner.revision)));
    repo.delete_policy_revision(&winner.policy_id, winner.revision)
        .unwrap();
    repo.put_policy(&principal_policy("replacement")).unwrap();
}

#[test]
fn over_limit_v3_database_recovers_manual_work_and_can_be_shrunk() {
    struct Discovery;
    impl asc_pap::ScopeDiscovery for Discovery {
        fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), PapError> {
            Ok(())
        }
        fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
            Ok(())
        }
    }
    let dir = private_dir();
    let path = dir.path().join("policy-state.db");
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repo.put_policy(&binding.policy).unwrap();
    let scope = PreparedScope {
        scope_id: binding.scope.scope_id,
        selector: binding.scope.selector,
        policy_snapshots: vec![binding.policy],
        owner_uid: None,
        status: ScopeStatus::Active,
    };
    repo.put_scope(&scope).unwrap();
    repo.begin_scope_delete(&scope.scope_id).unwrap();
    drop(repo);

    let mut large = principal_policy("stored");
    let asc_policy_types::authoring::PolicySource::Cedar(source) = &mut large.template else {
        unreachable!()
    };
    // Multibyte UTF-8 makes an accidental count of characters fall below the 16 MiB limit.
    source.text.push_str("\n//");
    source.text.push_str(&"文".repeat(65_536));
    insert_historical_sources(&path, &large, 87);
    let repo = std::sync::Arc::new(SqlitePolicyRepository::open(&path).unwrap());
    assert_eq!(
        repo.automatic_policies(),
        Err(PapError::AutomaticPolicyCapacity)
    );
    let service = asc_pap::PapService::new(repo.clone())
        .with_scope_discovery(std::sync::Arc::new(Discovery))
        .with_automatic_scopes(Ok(std::collections::BTreeMap::new()), &[], || {});
    service.recover_scopes().unwrap();
    assert_eq!(repo.get_scope(&scope.scope_id), Err(PapError::NotFound));
    service.sweep_deleting_scopes().unwrap();
    assert_eq!(
        repo.put_policy(&principal_policy("new")),
        Err(PapError::AutomaticPolicyCapacity)
    );
    let mut smaller = principal_policy("stored-0001");
    smaller.revision = Revision::new(2).unwrap();
    repo.put_policy(&smaller).unwrap();
    assert_eq!(
        repo.automatic_policies(),
        Err(PapError::AutomaticPolicyCapacity)
    );
    repo.delete_policy_revision(
        &ResourceId::new("stored-0002").unwrap(),
        Revision::new(1).unwrap(),
    )
    .unwrap();
    repo.put_policy(&principal_policy("new")).unwrap();
    large.policy_id = smaller.policy_id.clone();
    large.revision = Revision::new(3).unwrap();
    assert_eq!(
        repo.put_policy(&large),
        Err(PapError::AutomaticPolicyCapacity)
    );
    assert_eq!(
        repo.get_policy(&smaller.policy_id, smaller.revision)
            .unwrap(),
        smaller
    );
}

#[test]
fn automatic_recovery_reports_corrupt_storage() {
    let repo = SqlitePolicyRepository::temporary().unwrap();
    // Opening validates every retained record. Inject the fault after a valid open
    // to exercise the PAP recovery paths themselves, including the periodic sweep.
    SqlitePolicyRepository::before_next_commit_for_test(|tx| {
        let updated = tx
            .execute(
                "UPDATE policies SET last_allocated_revision=2 WHERE policy_id='bad-head'",
                [],
            )
            .unwrap();
        assert_eq!(updated, 1);
    });
    repo.put_policy(&principal_policy("bad-head")).unwrap();
    let service = asc_pap::PapService::new(std::sync::Arc::new(repo)).with_automatic_scopes(
        Ok(std::collections::BTreeMap::new()),
        &[],
        || {},
    );
    assert_eq!(service.recover_scopes(), Err(PapError::Persistence));
    assert_eq!(service.sweep_deleting_scopes(), Err(PapError::Persistence));
}
