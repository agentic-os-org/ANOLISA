use asc_pap::PapRepository;
use asc_policy_repository::*;
use asc_policy_types::{
    binding::{BindingStatus, PreparedBinding},
    scope::{PreparedScope, ScopeStatus},
};
use std::os::unix::fs::PermissionsExt;

pub(super) trait Repository:
    PapRepository + BindingStateRepository + BindingReconcileCatalog
{
}
impl<T: PapRepository + BindingStateRepository + BindingReconcileCatalog> Repository for T {}

pub(super) fn seed(repo: &dyn Repository) -> BindingStateSnapshot {
    let binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repo.put_policy(&binding.policy).unwrap();
    repo.put_scope(&PreparedScope {
        scope_id: binding.scope.scope_id.clone(),
        selector: binding.scope.selector.clone(),
        policy_snapshots: vec![binding.policy],
        owner_uid: None,
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
pub(super) fn transition(
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
pub(super) fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
