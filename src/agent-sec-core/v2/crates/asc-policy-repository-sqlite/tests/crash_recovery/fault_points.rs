//! Run the same SQL scenarios normally and with SIGKILL at transaction boundaries.
use super::*;
use asc_policy_types::policy::PreparedPolicy;
use rusqlite::{Connection, OpenFlags};

struct Case {
    root: tempfile::TempDir,
    mock: Mock,
    crash: bool,
    // Release serialization only after the mock and all scenario resources are dropped.
    _scenario: std::sync::MutexGuard<'static, ()>,
}
impl Case {
    fn new(crash: bool) -> Self {
        let scenario = serial_scenario();
        let root = tempfile::Builder::new()
            .prefix("asc-policy-fault-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let mock = Mock::start(root.path());
        Self {
            root,
            mock,
            crash,
            _scenario: scenario,
        }
    }
    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.root.path().join("policy-state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn repository(&self) -> Arc<SqlitePolicyRepository> {
        let path = self.root.path().join("policy-state.db");
        let repo = Arc::new(SqlitePolicyRepository::open(&path).unwrap_or_else(|error| {
            panic!(
                "open {} failed: {error:?}\n{}",
                path.display(),
                std::backtrace::Backtrace::force_capture()
            )
        }));
        let db = self.db();
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(
            !db.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
        repo
    }
    fn seed(&self, policy_count: usize, bindings: bool) {
        let repo = self.repository();
        let scope = scope(policy_count);
        for policy in &scope.policy_snapshots {
            repo.put_policy(policy).unwrap();
        }
        repo.put_scope(&scope).unwrap();
        if bindings {
            repo.sync_scope_instances(&scope.scope_id, &[fixture().scope.process])
                .unwrap();
        }
    }
    fn run_at(&self, stage: &str) {
        eprintln!(
            "SQL case {stage}: {}",
            if self.crash { "crash" } else { "baseline" }
        );
        child_with_crash(
            self.root.path(),
            &self.mock.address,
            stage,
            false,
            false,
            self.crash,
        );
    }
    fn recover(&self, delete: bool) {
        child(self.root.path(), &self.mock.address, "none", false, delete);
    }
    fn assert_clean(&self) {
        let repo = self.repository();
        assert_eq!(repo.list_bindings(100, 0).unwrap().total, 0);
        assert_eq!(repo.list_scopes(100, 0).unwrap().total, 0);
        assert!(self.mock.present().is_empty());
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.root.disable_cleanup(true);
            eprintln!("case artifacts retained at {}", self.root.path().display());
        }
    }
}

fn scope(policy_count: usize) -> PreparedScope {
    let input = fixture();
    let mut policies = vec![input.policy.clone()];
    if policy_count == 2 {
        policies.push(PreparedPolicy {
            policy_id: ResourceId::new("00000000-0000-4000-8000-000000000003").unwrap(),
            policy_name: "second policy".into(),
            ..input.policy
        });
    }
    PreparedScope {
        scope_id: input.scope.scope_id,
        selector: input.scope.selector,
        policy_snapshots: policies,
        status: ScopeStatus::Active,
    }
}

#[cfg(feature = "fault-injection")]
fn arm(point: &str, stage: &str) {
    assert!(fail::has_failpoints());
    let stage = stage.to_owned();
    fail::cfg_callback(point, move || gate(&stage)).unwrap();
}

#[cfg(feature = "fault-injection")]
pub(super) fn before_cas(write: &BindingStateWrite) {
    if !crash_enabled() {
        return;
    }
    let Ok(stage) = std::env::var("ASC_POLICY_CRASH_STAGE") else {
        return;
    };
    let Some(deployments) = write
        .next
        .as_ref()
        .and_then(|patch| patch.deployments.as_ref())
    else {
        return;
    };
    let selected = match stage.as_str() {
        "sql_unknown_state_saved" => deployments.iter().any(|d| d.presence == Presence::Unknown),
        _ => false,
    };
    if selected {
        arm("policy.sqlite.cas.state_saved", &stage);
    }
}

fn claim_write(expected: &BindingStateSnapshot) -> BindingStateWrite {
    BindingStateWrite {
        write_id: uuid::Uuid::from_u128(1),
        next: Some(ReconciliationPatch {
            status: Some(BindingStatus::Applying.into()),
            deployments: Some(vec![Deployment {
                target: TargetRef {
                    route: "mock".into(),
                    id: expected.binding.spec.binding_id.to_string(),
                    cleanup: b"stable-cleanup-v1".to_vec(),
                },
                revision: expected.binding.spec.binding_revision,
                presence: Presence::Unknown,
                last_confirmed: None,
            }]),
        }),
    }
}

pub(super) fn run_child(repo: &Arc<SqlitePolicyRepository>, remote: &str) -> bool {
    let stage = std::env::var("ASC_POLICY_CRASH_STAGE").unwrap();
    let point = match stage.as_str() {
        "sql_scope_inserted" => Some("policy.sqlite.scope.inserted"),
        "sql_instances_pinned" => Some("policy.sqlite.instances.pinned"),
        "sql_instances_binding_inserted" => Some("policy.sqlite.instances.binding_inserted"),
        "sql_discovery_stopped" => Some("policy.sqlite.discovery.stopped"),
        "sql_discovery_binding_retired" => Some("policy.sqlite.discovery.binding_retired"),
        "sql_cas_state_saved" => Some("policy.sqlite.cas.state_saved"),
        "sql_cas_after_transaction" => Some("policy.sqlite.cas.after_transaction"),
        "sql_binding_deleted" => Some("policy.sqlite.cas.binding_deleted"),
        "sql_scope_finalized" => Some("policy.sqlite.cas.scope_finalized"),
        // The wrapper arms this only for target registration, after the claim commits.
        "sql_unknown_state_saved" => None,
        _ => return false,
    };
    #[cfg(feature = "fault-injection")]
    if let (true, Some(point)) = (crash_enabled(), point) {
        arm(point, &stage);
    }
    #[cfg(not(feature = "fault-injection"))]
    assert!(
        !crash_enabled(),
        "SQL crash window {stage} ({point:?}) requires fault-injection"
    );
    match stage.as_str() {
        "sql_scope_inserted" => {
            repo.put_scope(&scope(1)).unwrap();
        }
        "sql_instances_pinned" | "sql_instances_binding_inserted" => {
            repo.sync_scope_instances(&fixture().scope.scope_id, &[fixture().scope.process])
                .unwrap();
        }
        "sql_discovery_stopped" | "sql_discovery_binding_retired" => {
            repo.finish_scope_discovery(&fixture().scope.scope_id)
                .unwrap();
        }
        "sql_cas_state_saved" | "sql_cas_after_transaction" => {
            let bindings = repo.list_bindings(100, 0).unwrap();
            let expected = repo
                .get_binding_state(&bindings.items[0].spec.binding_id)
                .unwrap()
                .unwrap();
            repo.compare_exchange_binding_state(&expected, &claim_write(&expected))
                .unwrap();
        }
        _ => converge(repo, remote),
    }
    assert!(
        !crash_enabled(),
        "SQL crash window was not reached: {stage}"
    );
    true
}

#[test]
fn baseline_scope_admission() {
    scope_admission(false);
}

#[cfg(feature = "fault-injection")]
#[test]
fn uncommitted_scope_is_not_an_accepted_assignment() {
    scope_admission(true);
}

fn scope_admission(crash: bool) {
    let case = Case::new(crash);
    let repo = case.repository();
    repo.put_policy(&fixture().policy).unwrap();
    drop(repo);
    case.run_at("sql_scope_inserted");
    if crash {
        case.assert_clean();
        case.recover(false);
        case.assert_clean();
    }
    assert!(case.mock.calls.lock().unwrap().is_empty());
    let repo = case.repository();
    assert_eq!(
        repo.get_policy(&fixture().policy.policy_id, fixture().policy.revision)
            .unwrap(),
        fixture().policy
    );
    if crash {
        repo.put_scope(&scope(1)).unwrap();
    } else {
        assert_eq!(repo.get_scope(&fixture().scope.scope_id).unwrap(), scope(1));
        assert_eq!(repo.list_bindings(100, 0).unwrap().total, 0);
    }
    drop(repo);
    case.recover(true);
    case.assert_clean();
    assert_eq!(*case.mock.calls.lock().unwrap(), ["apply", "delete"]);
}

#[test]
fn baseline_pin_and_multi_policy_expansion() {
    pin_and_multi_policy_expansion(false);
}

#[cfg(feature = "fault-injection")]
#[test]
fn pin_and_multi_policy_expansion_roll_back_together() {
    pin_and_multi_policy_expansion(true);
}

fn pin_and_multi_policy_expansion(crash: bool) {
    for stage in ["sql_instances_pinned", "sql_instances_binding_inserted"] {
        let case = Case::new(crash);
        case.seed(2, false);
        case.run_at(stage);
        let repo = case.repository();
        let seed = repo
            .scope_discovery_seed(&fixture().scope.scope_id)
            .unwrap();
        assert_eq!(seed.scope, scope(2), "{stage}");
        let pending = repo.list_bindings(100, 0).unwrap();
        if crash {
            assert_eq!(seed.pinned_process, None, "{stage}");
            assert!(seed.instances.is_empty(), "{stage}");
            assert_eq!(pending.total, 0, "{stage}");
        } else {
            assert_eq!(
                seed.pinned_process,
                Some(fixture().scope.process),
                "{stage}"
            );
            assert_eq!(seed.instances.len(), 1, "{stage}");
            assert_eq!(pending.total, 2, "{stage}");
            assert!(
                pending
                    .items
                    .iter()
                    .all(|b| b.status.phase == BindingStatus::PendingApply)
            );
        }
        assert!(case.mock.calls.lock().unwrap().is_empty(), "{stage}");
        drop(repo);
        case.recover(false);
        let repo = case.repository();
        let bindings = repo.list_bindings(100, 0).unwrap();
        assert_eq!(bindings.total, 2, "{stage}");
        if !crash {
            assert_eq!(
                bindings.items.iter().map(|b| &b.spec).collect::<Vec<_>>(),
                pending.items.iter().map(|b| &b.spec).collect::<Vec<_>>(),
                "{stage}"
            );
        }
        assert!(
            bindings
                .items
                .iter()
                .all(|b| b.status.phase == BindingStatus::Ready)
        );
        assert_eq!(
            bindings
                .items
                .iter()
                .map(|b| b.spec.policy.policy_id.clone())
                .collect::<BTreeSet<_>>(),
            scope(2)
                .policy_snapshots
                .iter()
                .map(|p| p.policy_id.clone())
                .collect()
        );
        assert_eq!(
            repo.scope_discovery_seed(&fixture().scope.scope_id)
                .unwrap()
                .pinned_process,
            Some(fixture().scope.process)
        );
        assert_eq!(case.mock.present().len(), 2);
        drop(repo);
        case.recover(false);
        let repo = case.repository();
        assert_eq!(repo.list_bindings(100, 0).unwrap().items, bindings.items);
        assert_eq!(*case.mock.calls.lock().unwrap(), ["apply", "apply"]);
        drop(repo);
        case.recover(true);
        case.assert_clean();
        assert_eq!(
            *case.mock.calls.lock().unwrap(),
            ["apply", "apply", "delete", "delete"]
        );
    }
}

#[test]
fn baseline_discovery_stop_and_binding_retirement() {
    discovery_stop_and_binding_retirement(false);
}

#[cfg(feature = "fault-injection")]
#[test]
fn discovery_stop_and_binding_retirement_are_atomic() {
    discovery_stop_and_binding_retirement(true);
}

fn discovery_stop_and_binding_retirement(crash: bool) {
    for stage in ["sql_discovery_stopped", "sql_discovery_binding_retired"] {
        let case = Case::new(crash);
        case.seed(2, true);
        let repo = case.repository();
        let original = repo.list_bindings(100, 0).unwrap().items;
        let snapshots: Vec<_> = original
            .iter()
            .map(|b| repo.get_binding_state(&b.spec.binding_id).unwrap().unwrap())
            .collect();
        repo.begin_scope_delete(&fixture().scope.scope_id).unwrap();
        drop(repo);
        case.run_at(stage);
        let repo = case.repository();
        assert_eq!(
            repo.get_scope(&fixture().scope.scope_id).unwrap().status,
            ScopeStatus::Deleting
        );
        assert_eq!(
            case.db()
                .query_row("SELECT discovery_stopped FROM scopes", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            i64::from(!crash)
        );
        let retired = repo.list_bindings(100, 0).unwrap().items;
        assert_eq!(retired.len(), original.len(), "{stage}");
        if crash {
            assert_eq!(retired, original, "{stage}");
        }
        for snapshot in snapshots {
            let saved = repo
                .get_binding_state(&snapshot.binding.spec.binding_id)
                .unwrap()
                .unwrap();
            if crash {
                assert_eq!(saved, snapshot, "{stage}");
            } else {
                assert_eq!(saved.binding.spec, snapshot.binding.spec, "{stage}");
                assert_eq!(
                    saved.binding.status.phase,
                    BindingStatus::PendingDelete,
                    "{stage}"
                );
                assert_eq!(saved.status_version, snapshot.status_version + 1, "{stage}");
                assert_eq!(saved.deployments, snapshot.deployments, "{stage}");
            }
        }
        drop(repo);
        case.recover(false);
        case.assert_clean();
        assert!(
            case.mock.calls.lock().unwrap().is_empty(),
            "retired bindings must never Apply"
        );
    }
}

#[test]
fn baseline_cas_state_and_write_receipt() {
    cas_state_and_write_receipt(false);
}

#[cfg(feature = "fault-injection")]
#[test]
fn cas_state_and_write_receipt_commit_together_and_replay_once() {
    cas_state_and_write_receipt(true);
}

fn cas_state_and_write_receipt(crash: bool) {
    for stage in ["sql_cas_state_saved", "sql_cas_after_transaction"] {
        let case = Case::new(crash);
        case.seed(1, true);
        let repo = case.repository();
        let id = repo.list_bindings(100, 0).unwrap().items[0]
            .spec
            .binding_id
            .clone();
        let expected = repo.get_binding_state(&id).unwrap().unwrap();
        let write = claim_write(&expected);
        let receipt = WriteReceipt {
            status_version: Some(expected.status_version + 1),
            status_applied: true,
        };
        drop(repo);
        case.run_at(stage);
        let repo = case.repository();
        let saved = repo.get_binding_state(&id).unwrap().unwrap();
        let row: (Option<String>, Option<Vec<u8>>, Option<String>) = case
            .db()
            .query_row(
                "SELECT last_write_id,last_write_digest,last_write_result_json FROM bindings",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let committed = !crash || stage == "sql_cas_after_transaction";
        if committed {
            assert_eq!(saved.binding.spec, expected.binding.spec);
            assert_eq!(saved.binding.status.phase, BindingStatus::Applying);
            assert_eq!(saved.status_version, expected.status_version + 1);
            assert_eq!(
                saved.deployments,
                write.next.as_ref().unwrap().deployments.clone().unwrap()
            );
            assert_eq!(
                row,
                (
                    Some(write.write_id.to_string()),
                    Some(write_digest(&expected, &write).unwrap().to_vec()),
                    Some(serde_json::to_string(&receipt).unwrap())
                )
            );
        } else {
            assert_eq!(saved, expected);
            assert_eq!(row, (None, None, None));
        }
        assert!(case.mock.calls.lock().unwrap().is_empty());
        assert_eq!(
            repo.compare_exchange_binding_state(&expected, &write)
                .unwrap(),
            if committed {
                WriteResult::AlreadyApplied(receipt)
            } else {
                WriteResult::Applied(receipt)
            }
        );
        let after = repo.get_binding_state(&id).unwrap().unwrap();
        assert_eq!(
            repo.compare_exchange_binding_state(&expected, &write)
                .unwrap(),
            WriteResult::AlreadyApplied(receipt)
        );
        assert_eq!(repo.get_binding_state(&id).unwrap(), Some(after));
        drop(repo);
        case.recover(true);
        case.assert_clean();
        assert_eq!(*case.mock.calls.lock().unwrap(), ["apply", "delete"]);
    }
}

#[test]
fn baseline_unknown_before_remote_apply() {
    unknown_before_remote_apply(false);
}

#[cfg(feature = "fault-injection")]
#[test]
fn uncommitted_unknown_never_precedes_remote_apply() {
    unknown_before_remote_apply(true);
}

fn unknown_before_remote_apply(crash: bool) {
    let case = Case::new(crash);
    case.seed(1, true);
    case.run_at("sql_unknown_state_saved");
    let repo = case.repository();
    let bindings = repo.list_bindings(100, 0).unwrap();
    assert_eq!(bindings.total, 1);
    let saved = repo
        .get_binding_state(&bindings.items[0].spec.binding_id)
        .unwrap()
        .unwrap();
    if crash {
        assert_eq!(saved.binding.status.phase, BindingStatus::Applying);
        assert_eq!(saved.status_version, 2);
        assert!(saved.deployments.is_empty());
        assert!(case.mock.calls.lock().unwrap().is_empty());
        assert!(case.mock.present().is_empty());
    } else {
        assert_eq!(saved.binding.status.phase, BindingStatus::Ready);
        assert_eq!(saved.status_version, 3);
        assert_eq!(saved.deployments.len(), 1);
        assert_eq!(saved.deployments[0].presence, Presence::Present);
        assert_eq!(*case.mock.calls.lock().unwrap(), ["apply"]);
        assert_eq!(
            case.mock.present(),
            BTreeSet::from([saved.binding.spec.binding_id.to_string()])
        );
    }
    drop(repo);
    case.recover(false);
    case.recover(false);
    let repo = case.repository();
    assert_eq!(
        repo.list_bindings(100, 0).unwrap().items[0].status.phase,
        BindingStatus::Ready
    );
    assert_eq!(*case.mock.calls.lock().unwrap(), ["apply"]);
    drop(repo);
    case.recover(true);
    case.assert_clean();
    assert_eq!(*case.mock.calls.lock().unwrap(), ["apply", "delete"]);
}

#[test]
fn baseline_last_binding_and_scope_removal() {
    last_binding_and_scope_removal(false);
}

#[cfg(feature = "fault-injection")]
#[test]
fn last_binding_and_scope_removal_roll_back_together() {
    last_binding_and_scope_removal(true);
}

fn last_binding_and_scope_removal(crash: bool) {
    for stage in ["sql_binding_deleted", "sql_scope_finalized"] {
        let case = Case::new(crash);
        child(case.root.path(), &case.mock.address, "none", true, false);
        let repo = case.repository();
        let id = repo.list_bindings(100, 0).unwrap().items[0]
            .spec
            .binding_id
            .clone();
        repo.begin_scope_delete(&fixture().scope.scope_id).unwrap();
        repo.finish_scope_discovery(&fixture().scope.scope_id)
            .unwrap();
        drop(repo);
        case.run_at(stage);
        let repo = case.repository();
        if crash {
            assert_eq!(
                repo.get_scope(&fixture().scope.scope_id).unwrap().status,
                ScopeStatus::Deleting,
                "{stage}"
            );
            assert_eq!(repo.list_bindings(100, 0).unwrap().total, 1, "{stage}");
            let saved = repo.get_binding_state(&id).unwrap().unwrap();
            assert_eq!(saved.binding.status.phase, BindingStatus::Deleting);
            assert_eq!(saved.status_version, 5, "{stage}");
            assert_eq!(saved.deployments.len(), 1, "{stage}");
            assert_eq!(saved.deployments[0].presence, Presence::Unknown, "{stage}");
            assert_eq!(
                saved.deployments[0].last_confirmed,
                Some(Presence::Present),
                "{stage}"
            );
            assert_eq!(saved.deployments[0].target.id, id.as_str(), "{stage}");
        } else {
            assert_eq!(repo.list_bindings(100, 0).unwrap().total, 0, "{stage}");
            assert_eq!(repo.list_scopes(100, 0).unwrap().total, 0, "{stage}");
        }
        assert!(case.mock.present().is_empty());
        assert_eq!(*case.mock.calls.lock().unwrap(), ["apply", "delete"]);
        drop(repo);
        case.recover(false);
        case.recover(false);
        case.assert_clean();
        assert_eq!(
            *case.mock.calls.lock().unwrap(),
            if crash {
                vec!["apply", "delete", "delete"]
            } else {
                vec!["apply", "delete"]
            },
            "{stage}"
        );
    }
}
