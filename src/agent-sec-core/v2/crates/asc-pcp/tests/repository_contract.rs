//! Storage contract: aggregate CAS, PAP interoperability, and ambiguous-write replay.
use std::sync::{Arc, Barrier};

use asc_pap::PapRepository;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_pcp::*;
use asc_policy_types::binding::{BindingStatus, BindingView};

fn initial() -> BindingStateSnapshot {
    BindingStateSnapshot {
        binding: BindingView {
            spec: serde_json::from_str(include_str!(
                "../../asc-policy-types/tests/fixtures/prepared-binding.json"
            ))
            .unwrap(),
            status: (BindingStatus::PendingApply).into(),
        },
        deployments: vec![],
    }
}

#[test]
fn concurrent_writes_from_one_snapshot_have_exactly_one_winner() {
    let before = initial();
    let repo =
        Arc::new(ProcessLocalPapRepository::with_binding_states(vec![before.clone()]).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let writers: Vec<_> = (1..=2)
        .map(|attempts| {
            let repo = repo.clone();
            let barrier = barrier.clone();
            let before = before.clone();
            std::thread::spawn(move || {
                let mut next = before.clone();
                next.binding.status.phase = BindingStatus::Applying;
                next.binding.status.error = Some(Failure::new(
                    FailureKind::Retryable,
                    &format!("WRITER_{attempts}"),
                ));
                let write = BindingStateWrite::new(next);
                barrier.wait();
                (
                    repo.compare_exchange_binding_state(&before, &write)
                        .unwrap(),
                    write,
                )
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = writers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        results
            .iter()
            .filter(|(r, _)| *r == WriteResult::Applied)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|(r, _)| *r == WriteResult::Conflict)
            .count(),
        1
    );
    let winner = &results
        .iter()
        .find(|(r, _)| *r == WriteResult::Applied)
        .unwrap()
        .1;
    assert_eq!(
        repo.get_binding_state(&before.binding.spec.binding_id)
            .unwrap(),
        Some(BindingStateSnapshot {
            binding: BindingView {
                spec: before.binding.spec.clone(),
                status: winner.next.as_ref().unwrap().status.clone().unwrap()
            },
            deployments: winner.next.as_ref().unwrap().deployments.clone().unwrap(),
        })
    );
    assert_eq!(
        repo.get_binding(&before.binding.spec.binding_id).unwrap(),
        BindingView {
            spec: before.binding.spec.clone(),
            status: winner.next.as_ref().unwrap().status.clone().unwrap()
        }
    );
}

#[test]
fn status_error_and_deployment_changes_invalidate_stale_snapshot() {
    for deployments in [false, true] {
        let before = initial();
        let repo = ProcessLocalPapRepository::with_binding_states(vec![before.clone()]).unwrap();
        let mut next = before.clone();
        if deployments {
            next.deployments.push(Deployment {
                target: TargetRef {
                    route: "test".into(),
                    id: "opaque".into(),
                    cleanup: vec![1],
                },
                revision: before.binding.spec.binding_revision,
                presence: Presence::Unknown,
                last_confirmed: None,
            });
        } else {
            next.binding.status.error = Some(Failure::new(FailureKind::Retryable, "RETRY"));
        }
        let write = BindingStateWrite::new(next.clone());
        assert_eq!(
            repo.compare_exchange_binding_state(&before, &write)
                .unwrap(),
            WriteResult::Applied
        );
        let mut stale_next = before.clone();
        stale_next.binding.status.phase = BindingStatus::Applying;
        assert_eq!(
            repo.compare_exchange_binding_state(&before, &BindingStateWrite::new(stale_next))
                .unwrap(),
            WriteResult::Conflict
        );
        assert_eq!(
            repo.get_binding_state(&before.binding.spec.binding_id)
                .unwrap(),
            Some(next)
        );
    }
}

#[test]
fn replay_after_pap_write_is_acknowledged_without_overwriting_new_intent() {
    let before = initial();
    let repo = ProcessLocalPapRepository::with_binding_states(vec![before.clone()]).unwrap();
    let mut next = before.clone();
    next.binding.status.phase = BindingStatus::ApplyFailed;
    let write = BindingStateWrite::new(next);
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &write)
            .unwrap(),
        WriteResult::Applied
    );
    let new_intent = before.binding.clone();
    repo.update_binding(
        Some(&repo.get_binding(&before.binding.spec.binding_id).unwrap()),
        &new_intent,
    )
    .unwrap();
    let current = repo
        .get_binding_state(&before.binding.spec.binding_id)
        .unwrap();
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &write)
            .unwrap(),
        WriteResult::AlreadyApplied
    );
    let mut reused = write.clone();
    reused.next.as_mut().unwrap().status.as_mut().unwrap().error =
        Some(Failure::new(FailureKind::Rejected, "CHANGED"));
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &reused),
        Err(StoreError::Invalid)
    );
    assert_eq!(
        repo.get_binding_state(&before.binding.spec.binding_id)
            .unwrap(),
        current
    );
    assert_eq!(
        repo.get_binding(&before.binding.spec.binding_id).unwrap(),
        new_intent
    );
}

#[test]
fn reconciliation_patch_cannot_write_a_spec_or_change_identity() {
    let before = initial();
    let repo = ProcessLocalPapRepository::with_binding_states(vec![before.clone()]).unwrap();
    let mut next = before.clone();
    next.binding.spec.binding_id =
        asc_foundation_types::ResourceId::new("10000000-0000-4000-8000-000000000002").unwrap();
    next.binding.status.error = Some(Failure::new(FailureKind::Retryable, "RETRY"));
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &BindingStateWrite::new(next))
            .unwrap(),
        WriteResult::Applied
    );
    let current = repo
        .get_binding_state(&before.binding.spec.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.binding.spec, before.binding.spec);
    assert_eq!(
        current.binding.status.error,
        Some(Failure::new(FailureKind::Retryable, "RETRY"))
    );
}

#[test]
fn removal_is_atomic_replayable_and_stale_writes_cannot_resurrect_revision_one() {
    let mut before = initial();
    before.binding.spec.binding_revision = asc_foundation_types::Revision::new(1).unwrap();
    let repo = ProcessLocalPapRepository::with_binding_states(vec![before.clone()]).unwrap();
    let write = BindingStateWrite::delete();
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &write)
            .unwrap(),
        WriteResult::Applied
    );
    let id = &before.binding.spec.binding_id;
    assert_eq!(repo.get_binding_state(id).unwrap(), None);
    assert_eq!(repo.get_binding(id), Err(asc_pap::PapError::NotFound));
    assert!(repo.list_bindings(100, 0).unwrap().items.is_empty());
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &write)
            .unwrap(),
        WriteResult::AlreadyApplied
    );
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &BindingStateWrite::new(before.clone()))
            .unwrap(),
        WriteResult::Conflict
    );
    assert_eq!(
        repo.update_binding(Some(&before.binding), &before.binding),
        Err(asc_pap::PapError::NotFound)
    );
    assert_eq!(repo.get_binding_state(id).unwrap(), None);
}

#[test]
fn stale_removal_cannot_erase_a_newer_status_or_target_observation() {
    let before = initial();
    let repo = ProcessLocalPapRepository::with_binding_states(vec![before.clone()]).unwrap();
    let mut next = before.clone();
    next.binding.status.phase = BindingStatus::PendingDelete;
    next.binding.status.error = Some(Failure::new(FailureKind::Retryable, "TEST_NEW_OBSERVATION"));
    repo.compare_exchange_binding_state(&before, &BindingStateWrite::new(next.clone()))
        .unwrap();
    assert_eq!(
        repo.compare_exchange_binding_state(&before, &BindingStateWrite::delete())
            .unwrap(),
        WriteResult::Conflict
    );
    assert_eq!(
        repo.get_binding_state(&before.binding.spec.binding_id)
            .unwrap(),
        Some(next)
    );
}
