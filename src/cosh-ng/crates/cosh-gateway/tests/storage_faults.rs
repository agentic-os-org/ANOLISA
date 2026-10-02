//! Deterministic storage fault and restart-boundary acceptance tests.

#![cfg(debug_assertions)]

use cosh_gateway::storage::StoreError;
use cosh_gateway::storage::{OutboxIntent, SqliteTaskStore, TaskCommit};
use cosh_gateway_contracts::{
    common::{
        BoundedName, BoundedOpaque, ContractHeader, ContractSchema, Correlation, Digest,
        IdempotencyKey, TargetRef,
    },
    ids::{ActorId, DeliveryId, InstallationId, MessageId, TaskId},
    task::{TaskEvent, TaskEventEnvelope},
};
fn submitted(task_id: &TaskId, actor_id: &ActorId) -> TaskEventEnvelope {
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    TaskEventEnvelope {
        header: ContractHeader::new(
            ContractSchema::TaskEvent,
            MessageId::new(),
            100,
            correlation,
        ),
        task_id: task_id.clone(),
        revision: 1,
        event: TaskEvent::TaskSubmitted {
            intent_digest: Digest::parse("a".repeat(64)).unwrap(),
            target: TargetRef {
                kind: BoundedName::new("local").unwrap(),
                authority: BoundedName::new("storage-fault-test").unwrap(),
                identifier: BoundedOpaque::new("target").unwrap(),
            },
        },
    }
}

fn commit_with_payload(
    task_id: &TaskId,
    actor_id: &ActorId,
    key: &str,
    delivery_id: DeliveryId,
    payload: serde_json::Value,
) -> TaskCommit {
    let event = submitted(task_id, actor_id);
    TaskCommit {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        command_digest: Digest::parse("b".repeat(64)).unwrap(),
        expected_revision: Some(0),
        outbox: vec![OutboxIntent {
            delivery_id,
            event_id: event.header.message_id.clone(),
            delivery_kind: BoundedName::new("task_event").unwrap(),
            payload,
            next_attempt_at_ms: 100,
        }],
        events: vec![event],
        committed_at_ms: 100,
    }
}

#[test]
fn sqlite_full_rolls_back_task_event_receipt_and_outbox() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway/state.db");
    let mut store = SqliteTaskStore::open(&path).unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let idempotency_key = IdempotencyKey::new("disk-full-command").unwrap();
    let command_digest = Digest::parse("b".repeat(64)).unwrap();
    let delivery_kind = BoundedName::new("task_event").unwrap();
    let worker = BoundedOpaque::new("storage-fault-worker").unwrap();

    assert!(store.freeze_database_growth_for_test().unwrap() > 0);

    let commit = commit_with_payload(
        &task_id,
        &actor_id,
        idempotency_key.as_str(),
        DeliveryId::new(),
        serde_json::json!({"body": "x".repeat(240 * 1024)}),
    );
    let error = store.commit_task_for_test(&commit).unwrap_err();
    assert!(matches!(
        error,
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if code.extended_code == rusqlite::ffi::SQLITE_FULL
    ));

    assert!(matches!(
        store.load_task(&task_id),
        Err(StoreError::TaskNotFound)
    ));
    assert!(store
        .load_command_receipt(&actor_id, &idempotency_key, &command_digest)
        .unwrap()
        .is_none());
    assert!(store
        .claim_outbox(&delivery_kind, &worker, 200, 300)
        .unwrap()
        .is_none());

    drop(store);
    let mut reopened = SqliteTaskStore::open(&path).unwrap();
    assert!(matches!(
        reopened.load_task(&task_id),
        Err(StoreError::TaskNotFound)
    ));
    assert!(reopened
        .load_command_receipt(&actor_id, &idempotency_key, &command_digest)
        .unwrap()
        .is_none());
    assert!(reopened
        .claim_outbox(&delivery_kind, &worker, 200, 300)
        .unwrap()
        .is_none());
}

#[test]
fn storage_fault_control_is_debug_only_and_has_no_fault_input() {
    let module_source = include_str!("../src/storage.rs");
    let harness_source = include_str!("../src/storage/fault_harness.rs");
    let task_store_owner_source = include_str!("../src/storage/task_store.rs");
    let task_store_commit_source = include_str!("../src/storage/task_store/commit.rs");
    assert!(module_source.contains("#[cfg(debug_assertions)]\nmod fault_harness;"));
    assert!(module_source.contains(
        "#[cfg(debug_assertions)]\n#[doc(hidden)]\npub use task_store::{OutboxIntent, TaskCommit};"
    ));
    assert!(module_source.contains(
        "#[cfg(not(debug_assertions))]\npub(crate) use task_store::{OutboxIntent, TaskCommit};"
    ));
    assert!(task_store_owner_source.contains("include!(\"task_store/commit.rs\");"));
    assert!(task_store_commit_source.contains("pub(crate) fn commit_task("));
    assert!(task_store_commit_source.contains(
        "#[cfg(debug_assertions)]\n    #[doc(hidden)]\n    pub fn commit_task_for_test("
    ));
    assert!(harness_source
        .contains("pub fn freeze_database_growth_for_test(&mut self) -> Result<u64, StoreError>"));
}

#[test]
fn expired_outbox_lease_reopens_with_same_delivery_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway/state.db");
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let delivery_id = DeliveryId::new();
    let delivery_kind = BoundedName::new("task_event").unwrap();
    let first_worker = BoundedOpaque::new("worker-before-crash").unwrap();
    let replacement_worker = BoundedOpaque::new("worker-after-restart").unwrap();

    {
        let mut store = SqliteTaskStore::open(&path).unwrap();
        store
            .commit_task_for_test(&commit_with_payload(
                &task_id,
                &actor_id,
                "outbox-before-crash",
                delivery_id.clone(),
                serde_json::json!({"event": "deliver-me"}),
            ))
            .unwrap();
        let claim = store
            .claim_outbox(&delivery_kind, &first_worker, 100, 200)
            .unwrap()
            .unwrap();
        assert_eq!(claim.delivery_id, delivery_id);
        assert_eq!(claim.attempt, 1);
    }

    {
        let mut reopened = SqliteTaskStore::open(&path).unwrap();
        assert!(reopened
            .claim_outbox(&delivery_kind, &replacement_worker, 199, 300)
            .unwrap()
            .is_none());
        let takeover = reopened
            .claim_outbox(&delivery_kind, &replacement_worker, 200, 300)
            .unwrap()
            .unwrap();
        assert_eq!(takeover.delivery_id, delivery_id);
        assert_eq!(takeover.attempt, 2);
        reopened.complete_outbox(&takeover, 250).unwrap();
    }

    let mut reopened = SqliteTaskStore::open(&path).unwrap();
    assert!(reopened
        .claim_outbox(&delivery_kind, &first_worker, 400, 500)
        .unwrap()
        .is_none());
}
