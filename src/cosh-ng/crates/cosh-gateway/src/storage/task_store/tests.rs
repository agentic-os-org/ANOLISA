use std::path::Path;
use std::{fs, os::unix::fs::PermissionsExt};

use cosh_gateway_contracts::common::{
    BoundedOpaque, BoundedText, ContractHeader, ContractSchema, Correlation, RuntimeSelector,
    TargetRef,
};
use cosh_gateway_contracts::ids::{InstallationId, RequestId, RunId};
use cosh_gateway_contracts::task::{RuntimeUpdate, SuspensionCode, TaskEvent};

use super::*;
use crate::storage::schema;
use tempfile::TempDir;

fn envelope(
    task_id: &TaskId,
    actor_id: &ActorId,
    revision: u64,
    event: TaskEvent,
) -> TaskEventEnvelope {
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    TaskEventEnvelope {
        header: ContractHeader::new(
            ContractSchema::TaskEvent,
            MessageId::new(),
            revision,
            correlation,
        ),
        task_id: task_id.clone(),
        revision,
        event,
    }
}

fn submitted(task_id: &TaskId, actor_id: &ActorId) -> TaskEventEnvelope {
    envelope(
        task_id,
        actor_id,
        1,
        TaskEvent::TaskSubmitted {
            intent_digest: Digest::parse("a".repeat(64)).unwrap(),
            target: TargetRef {
                kind: BoundedName::new("local").unwrap(),
                authority: BoundedName::new("test").unwrap(),
                identifier: BoundedOpaque::new("target").unwrap(),
            },
        },
    )
}

fn task_commit(
    task_id: &TaskId,
    actor_id: &ActorId,
    key: &str,
    digest: char,
    events: Vec<TaskEventEnvelope>,
    outbox: Vec<OutboxIntent>,
) -> TaskCommit {
    let _ = task_id;
    TaskCommit {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        command_digest: Digest::parse(digest.to_string().repeat(64)).unwrap(),
        expected_revision: Some(events.first().map_or(0, |event| event.revision - 1)),
        events,
        outbox,
        committed_at_ms: 100,
    }
}

fn outbox(event: &TaskEventEnvelope, delivery_id: DeliveryId) -> OutboxIntent {
    OutboxIntent {
        delivery_id,
        event_id: event.header.message_id.clone(),
        delivery_kind: BoundedName::new("task_event").unwrap(),
        payload: serde_json::json!({"event_id": event.header.message_id}),
        next_attempt_at_ms: 100,
    }
}

fn table_count(store: &SqliteTaskStore, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    store
        .connection()
        .query_row(&query, [], |row| row.get(0))
        .unwrap()
}

#[test]
fn task_snapshot_inventory_and_switch_state_are_task_scoped() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let task_id = TaskId::new();
    let submitted = submitted(&task_id, &actor_id);
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "snapshot-inventory-task",
            'a',
            vec![submitted],
            vec![],
        ))
        .unwrap();
    let run_id = RunId::new();
    let baseline_id = CheckpointId::new();
    let approval_id = cosh_gateway_contracts::ids::ApprovalId::new();
    let approval_checkpoint_id = CheckpointId::new();
    store.connection_mut().execute(
        "INSERT INTO pre_runtime_baselines(task_id,run_id,baseline_id,policy,state,evidence_json,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,'on','created','{}',1,1)",
        params![task_id.as_str(), run_id.as_str(), baseline_id.as_str()],
    ).unwrap();
    store
        .connection_mut()
        .execute(
            "INSERT INTO approvals(approval_id,request_id,actor_id,task_id,run_id,target_json,
                               operation_digest,input_digest,state,revision,expires_at_ms,
                               created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,?4,?5,'{}',?6,?7,'pending',1,100,1,1)",
            params![
                approval_id.as_str(),
                RequestId::new().as_str(),
                actor_id.as_str(),
                task_id.as_str(),
                run_id.as_str(),
                "d".repeat(64),
                "e".repeat(64)
            ],
        )
        .unwrap();
    store.connection_mut().execute(
        "INSERT INTO approval_checkpoint_barriers(approval_id,task_id,run_id,checkpoint_id,policy,runtime_fence_json,state,evidence_json,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,?4,'on','{}','created','{}',2,2)",
        params![approval_id.as_str(), task_id.as_str(), run_id.as_str(), approval_checkpoint_id.as_str()],
    ).unwrap();

    let inventory = store.load_task_snapshots(&task_id).unwrap();
    assert_eq!(inventory.len(), 2);
    assert_eq!(inventory[0].snapshot_id, baseline_id);
    assert_eq!(inventory[1].snapshot_id, approval_checkpoint_id);

    let key = IdempotencyKey::new("snapshot-switch-test").unwrap();
    let digest = Digest::parse("b".repeat(64)).unwrap();
    let preview = Digest::parse("c".repeat(64)).unwrap();
    let recovery = CheckpointId::new();
    store
        .record_task_snapshot_switch_intent(
            &actor_id,
            &key,
            &digest,
            &task_id,
            &baseline_id,
            &preview,
            1,
            &recovery,
            3,
        )
        .unwrap();
    let record = store
        .load_task_snapshot_switch(&actor_id, &key)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, "intent");
    assert_eq!(record.recovery_snapshot_id, recovery);
    assert_eq!(store.load_task_snapshots(&task_id).unwrap().len(), 2);
    store
        .transition_task_snapshot_switch(
            &actor_id,
            &key,
            "intent",
            "recovery_created",
            None,
            None,
            4,
        )
        .unwrap();
    assert_eq!(
        store
            .load_task_snapshot_switch(&actor_id, &key)
            .unwrap()
            .unwrap()
            .state,
        "recovery_created"
    );
    let mut from = "recovery_created";
    for to in ["switch_started", "unknown"] {
        let snapshots = store.load_task_snapshots(&task_id).unwrap();
        assert_eq!(snapshots.last().unwrap().snapshot_id, recovery);
        assert_eq!(snapshots.len(), 3);
        assert!(store
            .load_task_snapshots(&TaskId::new())
            .unwrap()
            .is_empty());
        store
            .transition_task_snapshot_switch(&actor_id, &key, from, to, None, None, 5)
            .unwrap();
        from = to;
    }
    assert_eq!(
        store
            .load_task_snapshots(&task_id)
            .unwrap()
            .last()
            .unwrap()
            .snapshot_id,
        recovery
    );
}

fn assert_task_commit_tables_empty(store: &SqliteTaskStore) {
    assert_eq!(table_count(store, "tasks"), 0);
    assert_eq!(table_count(store, "task_events"), 0);
    assert_eq!(table_count(store, "command_receipts"), 0);
    assert_eq!(table_count(store, "outbox"), 0);
}

fn running_event_batch(
    task_id: &TaskId,
    actor_id: &ActorId,
    count: usize,
) -> Vec<TaskEventEnvelope> {
    assert!(count >= 3);
    let run_id = RunId::new();
    let mut events = vec![
        submitted(task_id, actor_id),
        envelope(
            task_id,
            actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            task_id,
            actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
    ];
    for revision in 4..=u64::try_from(count).unwrap() {
        events.push(envelope(
            task_id,
            actor_id,
            revision,
            TaskEvent::RuntimeEventRecorded {
                run_id: run_id.clone(),
                update: RuntimeUpdate::Progress {
                    summary: BoundedText::new("bounded progress").unwrap(),
                },
            },
        ));
    }
    events
}

fn retry_guard_fixture() -> (SqliteTaskStore, TaskId, ActorId, RunId, RunId, TaskCommit) {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let previous_run_id = RunId::new();
    let next_run_id = RunId::new();
    let events = vec![
        submitted(&task_id, &actor_id),
        envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: previous_run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: previous_run_id.clone(),
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::RunSuspended {
                run_id: previous_run_id.clone(),
                reason: SuspensionCode::RuntimeUnavailable,
            },
        ),
    ];
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "retry-guard-source",
            '9',
            events,
            Vec::new(),
        ))
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO run_leases(
                 run_id, task_id, actor_id, lease_owner, generation, revision,
                 expires_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 'expired-retry-owner', 1, 1, 100, 2)",
            params![
                previous_run_id.as_str(),
                task_id.as_str(),
                actor_id.as_str()
            ],
        )
        .unwrap();
    let retry = envelope(
        &task_id,
        &actor_id,
        5,
        TaskEvent::RunRetryQueued {
            previous_run_id: previous_run_id.clone(),
            next_run_id: next_run_id.clone(),
        },
    );
    let commit = task_commit(
        &task_id,
        &actor_id,
        "retry-guard",
        'a',
        vec![retry.clone()],
        vec![OutboxIntent {
            delivery_id: DeliveryId::new(),
            event_id: retry.header.message_id,
            delivery_kind: BoundedName::new("runtime_start").unwrap(),
            payload: serde_json::json!({
                "actor": { "actor_id": actor_id.as_str() },
                "task_id": task_id.as_str(),
                "run_id": next_run_id.as_str(),
            }),
            next_attempt_at_ms: 100,
        }],
    );
    (
        store,
        task_id,
        actor_id,
        previous_run_id,
        next_run_id,
        commit,
    )
}

#[test]
fn v3_queued_task_without_runtime_intent_converges_once_without_launch() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("gateway-v3.db");
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();

    {
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        schema::migrate_to_for_test(&mut connection, 3).unwrap();
        let submitted = submitted(&task_id, &actor_id);
        let mut queued = envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("acp").unwrap(),
                    profile: Some(BoundedName::new("codex").unwrap()),
                },
            },
        );
        queued.header.correlation.installation_id =
            submitted.header.correlation.installation_id.clone();
        queued.header.correlation.run_id = Some(run_id.clone());
        queued.header.correlation.causation_message_id = Some(submitted.header.message_id.clone());
        let aggregate = TaskAggregate::replay(&[submitted.clone(), queued.clone()]).unwrap();
        let transaction = connection.transaction().unwrap();
        persist_projection(&transaction, &aggregate, 0, 100).unwrap();
        append_events(&transaction, &[submitted, queued]).unwrap();
        let occupied_key = format!("legacy-runtime-start-recovery:{}", task_id.as_str());
        let occupied_digest = "f".repeat(64);
        let occupied_receipt = CommitReceipt {
            task_id: task_id.clone(),
            revision: 2,
            event_ids: Vec::new(),
            delivery_ids: Vec::new(),
        };
        transaction
            .execute(
                "INSERT INTO command_receipts(
                     actor_id, idempotency_key, command_digest, task_id,
                     task_revision, receipt_json, committed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 2, ?5, 100)",
                params![
                    actor_id.as_str(),
                    occupied_key,
                    occupied_digest,
                    task_id.as_str(),
                    serde_json::to_string(&occupied_receipt).unwrap(),
                ],
            )
            .unwrap();
        transaction.commit().unwrap();

        let versions = connection
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get::<_, u32>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(versions, [1, 2, 3]);
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    {
        let mut store = SqliteTaskStore::open(&path).unwrap();
        let recovered = store.load_task(&task_id).unwrap();
        assert_eq!(recovered.state(), TaskState::Cancelled);
        assert_eq!(recovered.revision(), 5);
        let events = load_events(store.connection(), &task_id).unwrap();
        assert!(matches!(
            events[2].event,
            TaskEvent::CancellationRequested {
                cause: CancelReason::RuntimeShutdown,
                ..
            }
        ));
        assert!(matches!(
            events[3].event,
            TaskEvent::RunCancelled {
                stage: CancellationStage::BeforeRuntime,
                ..
            }
        ));
        assert!(matches!(events[4].event, TaskEvent::TaskCancelled));
        let recovery: (String, i64, String, String) = store
            .connection()
            .query_row(
                "SELECT state, settled_revision, settlement_digest,
                        settlement_event_ids_json
                 FROM legacy_runtime_start_recoveries WHERE task_id=?1",
                params![task_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(recovery.0, "settled");
        assert_eq!(recovery.1, 5);
        assert_eq!(recovery.2.len(), 64);
        let settlement_event_ids: Vec<MessageId> = serde_json::from_str(&recovery.3).unwrap();
        assert_eq!(settlement_event_ids.len(), 3);
        assert_eq!(table_count(&store, "command_receipts"), 1);
        let occupied_receipt: (String, String, i64) = store
            .connection()
            .query_row(
                "SELECT command_digest, receipt_json, task_revision
                 FROM command_receipts
                 WHERE actor_id=?1 AND idempotency_key=?2",
                params![
                    actor_id.as_str(),
                    format!("legacy-runtime-start-recovery:{}", task_id.as_str())
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(occupied_receipt.0, "f".repeat(64));
        assert_eq!(occupied_receipt.2, 2);
        assert_eq!(
            serde_json::from_str::<CommitReceipt>(&occupied_receipt.1).unwrap(),
            CommitReceipt {
                task_id: task_id.clone(),
                revision: 2,
                event_ids: Vec::new(),
                delivery_ids: Vec::new(),
            }
        );
        assert_eq!(table_count(&store, "outbox"), 0);
        let runtime_start = BoundedName::new("runtime_start").unwrap();
        let worker = BoundedOpaque::new("upgrade-test-worker").unwrap();
        assert!(store
            .claim_outbox(&runtime_start, &worker, 200, 300)
            .unwrap()
            .is_none());
    }

    let store = SqliteTaskStore::open(&path).unwrap();
    let recovered = store.load_task(&task_id).unwrap();
    assert_eq!(recovered.state(), TaskState::Cancelled);
    assert_eq!(recovered.revision(), 5);
    assert_eq!(load_events(store.connection(), &task_id).unwrap().len(), 5);
    assert_eq!(table_count(&store, "command_receipts"), 1);
    let replayed_occupied_digest: String = store
        .connection()
        .query_row(
            "SELECT command_digest FROM command_receipts
             WHERE actor_id=?1 AND idempotency_key=?2",
            params![
                actor_id.as_str(),
                format!("legacy-runtime-start-recovery:{}", task_id.as_str())
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replayed_occupied_digest, "f".repeat(64));
    let replayed_marker: (String, i64) = store
        .connection()
        .query_row(
            "SELECT state, settled_revision
             FROM legacy_runtime_start_recoveries WHERE task_id=?1",
            params![task_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(replayed_marker, ("settled".to_owned(), 5));
}

#[test]
fn commits_projection_event_receipt_and_outbox_atomically() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    let delivery_id = DeliveryId::new();
    let commit = task_commit(
        &task_id,
        &actor_id,
        "create",
        'a',
        vec![event.clone()],
        vec![outbox(&event, delivery_id.clone())],
    );

    let outcome = store.commit_task(&commit).unwrap();
    let CommitOutcome::Applied(receipt) = outcome else {
        panic!("first commit must be applied")
    };
    assert_eq!(receipt.revision, 1);
    assert_eq!(receipt.delivery_ids, [delivery_id]);
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 1);
    assert_eq!(table_count(&store, "task_events"), 1);
    assert_eq!(table_count(&store, "command_receipts"), 1);
    assert_eq!(table_count(&store, "outbox"), 1);
}

#[test]
fn commit_count_bounds_accept_exact_limits_and_reject_overflow_without_mutation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let events = running_event_batch(&task_id, &actor_id, MAX_TASK_EVENTS_PER_COMMIT);
    let intents = (0..MAX_OUTBOX_INTENTS_PER_COMMIT)
        .map(|_| outbox(events.last().unwrap(), DeliveryId::new()))
        .collect();
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "exact-count-bounds",
            '1',
            events,
            intents,
        ))
        .unwrap();
    assert_eq!(table_count(&store, "task_events"), 64);
    assert_eq!(table_count(&store, "outbox"), 64);

    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let events = running_event_batch(&task_id, &actor_id, MAX_TASK_EVENTS_PER_COMMIT + 1);
    let commit = task_commit(
        &task_id,
        &actor_id,
        "event-count-overflow",
        '2',
        events,
        Vec::new(),
    );
    assert!(matches!(
        store.commit_task(&commit),
        Err(StoreError::InvalidCommit { message }) if message.contains("event batch exceeds")
    ));
    assert_task_commit_tables_empty(&store);

    let event = submitted(&task_id, &actor_id);
    let intents = (0..=MAX_OUTBOX_INTENTS_PER_COMMIT)
        .map(|_| outbox(&event, DeliveryId::new()))
        .collect();
    let commit = task_commit(
        &task_id,
        &actor_id,
        "outbox-count-overflow",
        '3',
        vec![event],
        intents,
    );
    assert!(matches!(
        store.commit_task(&commit),
        Err(StoreError::InvalidCommit { message }) if message.contains("Outbox batch exceeds")
    ));
    assert_task_commit_tables_empty(&store);
}

#[test]
fn payload_bound_accepts_exact_limit_and_rejects_one_more_without_mutation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    let payload = serde_json::Value::String("x".repeat(MAX_TASK_PAYLOAD_BYTES - 2));
    assert_eq!(
        serde_json::to_vec(&payload).unwrap().len(),
        MAX_TASK_PAYLOAD_BYTES
    );
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "exact-payload-bound",
            '4',
            vec![event.clone()],
            vec![OutboxIntent {
                delivery_id: DeliveryId::new(),
                event_id: event.header.message_id.clone(),
                delivery_kind: BoundedName::new("task_event").unwrap(),
                payload,
                next_attempt_at_ms: 100,
            }],
        ))
        .unwrap();

    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    let payload = serde_json::Value::String("x".repeat(MAX_TASK_PAYLOAD_BYTES - 1));
    assert_eq!(
        serde_json::to_vec(&payload).unwrap().len(),
        MAX_TASK_PAYLOAD_BYTES + 1
    );
    let commit = task_commit(
        &task_id,
        &actor_id,
        "payload-bound-overflow",
        '5',
        vec![event.clone()],
        vec![OutboxIntent {
            delivery_id: DeliveryId::new(),
            event_id: event.header.message_id.clone(),
            delivery_kind: BoundedName::new("task_event").unwrap(),
            payload,
            next_attempt_at_ms: 100,
        }],
    );
    assert!(matches!(
        store.commit_task(&commit),
        Err(StoreError::InvalidCommit { message }) if message.contains("Outbox payload exceeds")
    ));
    assert_task_commit_tables_empty(&store);
}

#[test]
fn internal_outbox_append_enforces_the_same_payload_bound_before_mutation() {
    fn create_submitted_task() -> (SqliteTaskStore, TaskId, ActorId) {
        let mut store = SqliteTaskStore::open_in_memory().unwrap();
        let task_id = TaskId::new();
        let actor_id = ActorId::new();
        store
            .commit_task(&task_commit(
                &task_id,
                &actor_id,
                "internal-bound-source",
                '8',
                vec![submitted(&task_id, &actor_id)],
                Vec::new(),
            ))
            .unwrap();
        (store, task_id, actor_id)
    }

    let (mut store, task_id, actor_id) = create_submitted_task();
    let run_id = RunId::new();
    let exact = serde_json::Value::String("x".repeat(MAX_TASK_PAYLOAD_BYTES - 2));
    let transaction = store.connection_mut().transaction().unwrap();
    append_internal_task_event(
        &transaction,
        &task_id,
        &actor_id,
        101,
        TaskEvent::TaskQueued {
            run_id,
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
        Some((BoundedName::new("internal").unwrap(), exact)),
    )
    .unwrap();
    transaction.commit().unwrap();
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 2);
    assert_eq!(table_count(&store, "outbox"), 1);

    let (mut store, task_id, actor_id) = create_submitted_task();
    let oversized = serde_json::Value::String("x".repeat(MAX_TASK_PAYLOAD_BYTES - 1));
    let transaction = store.connection_mut().transaction().unwrap();
    let error = append_internal_task_event(
        &transaction,
        &task_id,
        &actor_id,
        101,
        TaskEvent::TaskQueued {
            run_id: RunId::new(),
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
        Some((BoundedName::new("internal").unwrap(), oversized)),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StoreError::InvalidCommit { message } if message.contains("internal Outbox payload exceeds")
    ));
    drop(transaction);
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 1);
    assert_eq!(table_count(&store, "task_events"), 1);
    assert_eq!(table_count(&store, "outbox"), 0);
}

#[test]
fn aggregate_payload_bound_accepts_exact_limit_and_rejects_one_more_without_mutation() {
    fn exact_aggregate_commit(task_id: &TaskId, actor_id: &ActorId, key: &str) -> TaskCommit {
        let event = submitted(task_id, actor_id);
        let intents = (0..4)
            .map(|_| OutboxIntent {
                delivery_id: DeliveryId::new(),
                event_id: event.header.message_id.clone(),
                delivery_kind: BoundedName::new("task_event").unwrap(),
                payload: serde_json::Value::String(String::new()),
                next_attempt_at_ms: 100,
            })
            .collect();
        let mut commit = task_commit(task_id, actor_id, key, '6', vec![event], intents);
        let mut remaining = MAX_TASK_COMMIT_SERIALIZED_BYTES
            .checked_sub(serde_json::to_vec(&commit).unwrap().len())
            .unwrap();
        for intent in &mut commit.outbox {
            let serde_json::Value::String(value) = &mut intent.payload else {
                unreachable!()
            };
            let added = remaining.min(MAX_TASK_PAYLOAD_BYTES - 2);
            value.push_str(&"x".repeat(added));
            remaining -= added;
        }
        assert_eq!(remaining, 0);
        assert_eq!(
            serde_json::to_vec(&commit).unwrap().len(),
            MAX_TASK_COMMIT_SERIALIZED_BYTES
        );
        commit
    }

    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    store
        .commit_task(&exact_aggregate_commit(
            &task_id,
            &actor_id,
            "exact-aggregate-bound",
        ))
        .unwrap();

    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let mut commit = exact_aggregate_commit(&task_id, &actor_id, "aggregate-bound-overflow");
    let intent = commit
        .outbox
        .iter_mut()
        .find(|intent| serde_json::to_vec(&intent.payload).unwrap().len() < MAX_TASK_PAYLOAD_BYTES)
        .unwrap();
    let serde_json::Value::String(last) = &mut intent.payload else {
        unreachable!()
    };
    last.push('x');
    assert!(matches!(
        store.commit_task(&commit),
        Err(StoreError::InvalidCommit { message }) if message.contains("serialized commit exceeds")
    ));
    assert_task_commit_tables_empty(&store);
}

#[test]
fn outbox_claim_is_exclusive_and_stale_attempt_is_fenced() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("gateway.db");
    let mut first_store = SqliteTaskStore::open(&path).unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    first_store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "outbox-claim",
            'b',
            vec![event.clone()],
            vec![outbox(&event, DeliveryId::new())],
        ))
        .unwrap();
    let mut second_store = SqliteTaskStore::open(&path).unwrap();
    let kind = BoundedName::new("task_event").unwrap();
    let first_owner = BoundedOpaque::new("worker-a").unwrap();
    let second_owner = BoundedOpaque::new("worker-b").unwrap();

    let first = first_store
        .claim_outbox(&kind, &first_owner, 100, 200)
        .unwrap()
        .unwrap();
    assert_eq!(first.attempt, 1);
    assert!(second_store
        .claim_outbox(&kind, &second_owner, 150, 250)
        .unwrap()
        .is_none());

    let takeover = second_store
        .claim_outbox(&kind, &second_owner, 200, 300)
        .unwrap()
        .unwrap();
    assert_eq!(takeover.delivery_id, first.delivery_id);
    assert_eq!(takeover.attempt, 2);
    assert!(matches!(
        first_store.complete_outbox(&first, 210),
        Err(StoreError::GenerationFenced { .. })
    ));
    second_store.complete_outbox(&takeover, 210).unwrap();
    assert!(first_store
        .claim_outbox(&kind, &first_owner, 400, 500)
        .unwrap()
        .is_none());
}

#[test]
fn validated_outbox_candidate_never_falls_through_to_the_next_delivery() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("gateway.db");
    let mut validating_store = SqliteTaskStore::open(&path).unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    validating_store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "candidate-race",
            'f',
            vec![event.clone()],
            vec![
                outbox(&event, DeliveryId::new()),
                outbox(&event, DeliveryId::new()),
            ],
        ))
        .unwrap();
    let mut competing_store = SqliteTaskStore::open(&path).unwrap();
    let kind = BoundedName::new("task_event").unwrap();
    let validated = validating_store
        .peek_ready_outbox(&kind, 100)
        .unwrap()
        .unwrap();

    let competing_claim = competing_store
        .claim_outbox(&kind, &BoundedOpaque::new("worker-b").unwrap(), 100, 200)
        .unwrap()
        .unwrap();
    assert_eq!(competing_claim.delivery_id, validated.delivery_id);
    assert!(validating_store
        .claim_outbox_candidate(
            &kind,
            &validated,
            &BoundedOpaque::new("worker-a").unwrap(),
            100,
            200,
        )
        .unwrap()
        .is_none());

    let next = validating_store
        .peek_ready_outbox(&kind, 100)
        .unwrap()
        .unwrap();
    assert_ne!(next.delivery_id, validated.delivery_id);
    assert_eq!(next.attempt, 0);
}

#[test]
fn stale_validated_outbox_attempt_is_normal_contention() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("gateway.db");
    let mut validating_store = SqliteTaskStore::open(&path).unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    validating_store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "stale-candidate-attempt",
            '9',
            vec![event.clone()],
            vec![outbox(&event, DeliveryId::new())],
        ))
        .unwrap();
    let mut competing_store = SqliteTaskStore::open(&path).unwrap();
    let kind = BoundedName::new("task_event").unwrap();
    let stale = validating_store
        .peek_ready_outbox(&kind, 100)
        .unwrap()
        .unwrap();

    let competing_claim = competing_store
        .claim_outbox(&kind, &BoundedOpaque::new("worker-b").unwrap(), 100, 200)
        .unwrap()
        .unwrap();
    assert_eq!(competing_claim.attempt, 1);
    assert!(validating_store
        .claim_outbox_candidate(
            &kind,
            &stale,
            &BoundedOpaque::new("worker-a").unwrap(),
            200,
            300,
        )
        .unwrap()
        .is_none());

    let refreshed = validating_store
        .peek_ready_outbox(&kind, 200)
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.delivery_id, stale.delivery_id);
    assert_eq!(refreshed.attempt, 1);
}

#[test]
fn outbox_retry_preserves_identity_and_increments_attempt() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "outbox-retry",
            'c',
            vec![event.clone()],
            vec![outbox(&event, DeliveryId::new())],
        ))
        .unwrap();
    let kind = BoundedName::new("task_event").unwrap();
    let owner = BoundedOpaque::new("worker").unwrap();
    let first = store
        .claim_outbox(&kind, &owner, 100, 200)
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.retry_outbox(&first, 200, 250),
        Err(StoreError::GenerationFenced { .. })
    ));
    store.retry_outbox(&first, 150, 175).unwrap();
    assert!(store
        .claim_outbox(&kind, &owner, 174, 250)
        .unwrap()
        .is_none());
    let retry = store
        .claim_outbox(&kind, &owner, 175, 250)
        .unwrap()
        .unwrap();
    assert_eq!(retry.delivery_id, first.delivery_id);
    assert_eq!(retry.attempt, 2);
}

#[test]
fn retry_runtime_start_loader_requires_one_delivered_owner_bound_intent() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let submitted = submitted(&task_id, &actor_id);
    let queued = envelope(
        &task_id,
        &actor_id,
        2,
        TaskEvent::TaskQueued {
            run_id: run_id.clone(),
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
    );
    let payload = serde_json::json!({
        "schema_version": 1,
        "actor": { "actor_id": actor_id.as_str() },
        "task_id": task_id.as_str(),
        "run_id": run_id.as_str(),
    });
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "retry-source",
            '7',
            vec![submitted, queued.clone()],
            vec![OutboxIntent {
                delivery_id: DeliveryId::new(),
                event_id: queued.header.message_id.clone(),
                delivery_kind: BoundedName::new("runtime_start").unwrap(),
                payload: payload.clone(),
                next_attempt_at_ms: 100,
            }],
        ))
        .unwrap();

    assert!(matches!(
        store.load_runtime_start_intent_for_retry(&actor_id, &task_id, &run_id),
        Err(StoreError::LedgerConflict { message })
            if message.contains("delivered runtime start intent")
    ));
    assert!(matches!(
        store.load_runtime_start_intent_for_retry(&ActorId::new(), &task_id, &run_id),
        Err(StoreError::LedgerConflict { message }) if message.contains("does not own")
    ));

    let kind = BoundedName::new("runtime_start").unwrap();
    let worker = BoundedOpaque::new("retry-loader").unwrap();
    let claim = store
        .claim_outbox(&kind, &worker, 100, 200)
        .unwrap()
        .unwrap();
    store.complete_outbox(&claim, 150).unwrap();
    assert_eq!(
        store
            .load_runtime_start_intent_for_retry(&actor_id, &task_id, &run_id)
            .unwrap(),
        payload
    );

    store
        .connection()
        .execute(
            "UPDATE outbox SET payload_json='{'
             WHERE delivery_id=?1",
            params![claim.delivery_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.load_runtime_start_intent_for_retry(&actor_id, &task_id, &run_id),
        Err(StoreError::Corrupt { message }) if message.contains("malformed JSON")
    ));
    let payload_json = serde_json::to_string(&payload).unwrap();
    store
        .connection()
        .execute(
            "UPDATE outbox SET payload_json=?2 WHERE delivery_id=?1",
            params![claim.delivery_id.as_str(), payload_json],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO outbox(
                 delivery_id, task_id, event_id, delivery_kind, payload_json,
                 state, attempt, next_attempt_at_ms, created_at_ms, delivered_at_ms
             ) VALUES (?1, ?2, ?3, 'runtime_start', ?4, 'delivered', 1, 100, 101, 150)",
            params![
                DeliveryId::new().as_str(),
                task_id.as_str(),
                queued.header.message_id.as_str(),
                serde_json::to_string(&payload).unwrap(),
            ],
        )
        .unwrap();
    assert!(matches!(
        store.load_runtime_start_intent_for_retry(&actor_id, &task_id, &run_id),
        Err(StoreError::Corrupt { message }) if message.contains("multiple runtime start")
    ));

    assert!(matches!(
        store.load_runtime_start_intent_for_retry(&actor_id, &task_id, &RunId::new()),
        Err(StoreError::LedgerNotFound { .. })
    ));
}

#[test]
fn retry_commit_guard_binds_one_runtime_start_intent_without_partial_mutation() {
    let variants = ["missing", "kind", "actor", "task", "next-run"];
    for (index, variant) in variants.into_iter().enumerate() {
        let (mut store, task_id, _actor_id, previous_run_id, _next_run_id, mut commit) =
            retry_guard_fixture();
        commit.idempotency_key =
            IdempotencyKey::new(format!("retry-guard-invalid-{index}")).unwrap();
        commit.command_digest = Digest::parse(format!("{:x}", index + 1).repeat(64)).unwrap();
        match variant {
            "missing" => commit.outbox.clear(),
            "kind" => {
                commit.outbox[0].delivery_kind = BoundedName::new("task_event").unwrap();
            }
            "actor" => {
                commit.outbox[0].payload["actor"]["actor_id"] =
                    serde_json::Value::String(ActorId::new().to_string());
            }
            "task" => {
                commit.outbox[0].payload["task_id"] =
                    serde_json::Value::String(TaskId::new().to_string());
            }
            "next-run" => {
                commit.outbox[0].payload["run_id"] =
                    serde_json::Value::String(RunId::new().to_string());
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            store.commit_retry_task(&commit, &previous_run_id),
            Err(StoreError::LedgerConflict { .. })
        ));
        assert_eq!(
            store.load_task(&task_id).unwrap().revision(),
            4,
            "{variant}"
        );
        assert_eq!(table_count(&store, "outbox"), 0, "{variant}");
        assert_eq!(table_count(&store, "command_receipts"), 1, "{variant}");
    }

    let (mut store, task_id, _actor_id, previous_run_id, next_run_id, commit) =
        retry_guard_fixture();
    assert!(matches!(
        store.commit_retry_task(&commit, &previous_run_id).unwrap(),
        CommitOutcome::Applied(_)
    ));
    let task = store.load_task(&task_id).unwrap();
    assert_eq!(task.state(), TaskState::Queued);
    assert_eq!(task.active_run_id(), Some(&next_run_id));
    assert_eq!(table_count(&store, "outbox"), 1);
}

#[test]
fn retry_commit_guard_rejects_live_previous_lease_without_mutation() {
    let (mut store, task_id, _actor_id, previous_run_id, _next_run_id, commit) =
        retry_guard_fixture();
    store
        .connection()
        .execute(
            "UPDATE run_leases SET expires_at_ms=101 WHERE run_id=?1",
            params![previous_run_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.commit_retry_task(&commit, &previous_run_id),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 4);
    assert_eq!(table_count(&store, "outbox"), 0);
    assert_eq!(table_count(&store, "command_receipts"), 1);
}

#[test]
fn event_page_is_owner_scoped_and_sql_bounded() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let submitted = submitted(&task_id, &actor_id);
    let queued = envelope(
        &task_id,
        &actor_id,
        2,
        TaskEvent::TaskQueued {
            run_id,
            runtime: RuntimeSelector {
                runtime: BoundedName::new("acp").unwrap(),
                profile: None,
            },
        },
    );
    store
        .commit_task(&task_commit(
            &task_id,
            &actor_id,
            "page",
            'd',
            vec![submitted, queued],
            Vec::new(),
        ))
        .unwrap();

    let (first, revision) = store
        .load_task_events_for_owner(&task_id, &actor_id, None, 1)
        .unwrap();
    assert_eq!(revision, 2);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].revision, 1);
    assert!(matches!(
        store.load_task_events_for_owner(&task_id, &ActorId::new(), None, 1),
        Err(StoreError::TaskNotFound)
    ));
    assert!(matches!(
        store.load_task_events_for_owner(&task_id, &actor_id, None, 65),
        Err(StoreError::InvalidCommit { .. })
    ));
}

#[test]
fn idempotency_replays_same_digest_and_rejects_conflict() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    let mut commit = task_commit(
        &task_id,
        &actor_id,
        "same-key",
        'b',
        vec![event],
        Vec::new(),
    );

    let applied = store.commit_task(&commit).unwrap();
    assert!(matches!(applied, CommitOutcome::Applied(_)));
    commit.expected_revision = Some(99);
    let replayed = store.commit_task(&commit).unwrap();
    assert!(matches!(replayed, CommitOutcome::Replayed(_)));
    commit.command_digest = Digest::parse("c".repeat(64)).unwrap();
    assert!(matches!(
        store.commit_task(&commit),
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(table_count(&store, "task_events"), 1);
}

#[test]
fn revision_conflict_has_no_partial_rows() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let event = submitted(&task_id, &actor_id);
    let mut commit = task_commit(
        &task_id,
        &actor_id,
        "conflict",
        'd',
        vec![event],
        Vec::new(),
    );
    commit.expected_revision = Some(1);

    assert!(matches!(
        store.commit_task(&commit),
        Err(StoreError::RevisionConflict {
            expected: 1,
            actual: 0
        })
    ));
    assert_eq!(table_count(&store, "tasks"), 0);
    assert_eq!(table_count(&store, "task_events"), 0);
    assert_eq!(table_count(&store, "command_receipts"), 0);
}

#[test]
fn actor_substitution_cannot_append_or_create_partial_rows() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let owner = ActorId::new();
    let attacker = ActorId::new();
    let event = submitted(&task_id, &owner);
    let substituted_create = task_commit(
        &task_id,
        &attacker,
        "substitute-create",
        '3',
        vec![event],
        Vec::new(),
    );
    assert!(matches!(
        store.commit_task(&substituted_create),
        Err(StoreError::InvalidCommit { .. })
    ));
    assert_eq!(table_count(&store, "tasks"), 0);

    let create_event = submitted(&task_id, &owner);
    store
        .commit_task(&task_commit(
            &task_id,
            &owner,
            "owner-create",
            '4',
            vec![create_event],
            Vec::new(),
        ))
        .unwrap();
    let queued = envelope(
        &task_id,
        &attacker,
        2,
        TaskEvent::TaskQueued {
            run_id: RunId::new(),
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
    );
    assert!(matches!(
        store.commit_task(&task_commit(
            &task_id,
            &attacker,
            "substitute-append",
            '5',
            vec![queued],
            Vec::new(),
        )),
        Err(StoreError::InvalidCommit { .. })
    ));
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 1);
    assert_eq!(table_count(&store, "task_events"), 1);
    assert_eq!(table_count(&store, "command_receipts"), 1);
}

#[test]
fn failed_outbox_insert_rolls_back_task_append() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let initial = submitted(&task_id, &actor_id);
    let duplicate_delivery = DeliveryId::new();
    let create = task_commit(
        &task_id,
        &actor_id,
        "create",
        'e',
        vec![initial.clone()],
        vec![outbox(&initial, duplicate_delivery.clone())],
    );
    store.commit_task(&create).unwrap();

    let queued = envelope(
        &task_id,
        &actor_id,
        2,
        TaskEvent::TaskQueued {
            run_id: RunId::new(),
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
    );
    let append = task_commit(
        &task_id,
        &actor_id,
        "queue",
        'f',
        vec![queued.clone()],
        vec![outbox(&queued, duplicate_delivery)],
    );
    assert!(matches!(
        store.commit_task(&append),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 1);
    assert_eq!(table_count(&store, "task_events"), 1);
    assert_eq!(table_count(&store, "command_receipts"), 1);
    assert_eq!(table_count(&store, "outbox"), 1);
}

#[test]
fn recovers_projection_after_durable_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway/state.db");
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    {
        let mut store = SqliteTaskStore::open(&path).unwrap();
        let event = submitted(&task_id, &actor_id);
        let event_id = event.header.message_id.clone();
        store
            .commit_task(&task_commit(
                &task_id,
                &actor_id,
                "recover",
                '1',
                vec![event],
                Vec::new(),
            ))
            .unwrap();
        let mut queued = envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: RunId::new(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        );
        queued.header.correlation.causation_message_id = Some(event_id.clone());
        store
            .commit_task(&task_commit(
                &task_id,
                &actor_id,
                "queue-after-recover",
                '2',
                vec![queued],
                Vec::new(),
            ))
            .unwrap();
        let causation: Option<String> = store
            .connection()
            .query_row(
                "SELECT causation_id FROM task_events
                 WHERE task_id = ?1 AND revision = 2",
                params![task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(causation.as_deref(), Some(event_id.as_str()));
    }

    let store = SqliteTaskStore::open(Path::new(&path)).unwrap();
    let recovered = store.recover_task(&task_id).unwrap();
    assert_eq!(recovered.task_id(), &task_id);
    assert_eq!(recovered.revision(), 2);
    assert_eq!(recovered.state(), TaskState::Queued);
}

#[test]
fn normal_load_and_commit_reject_divergent_snapshot() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let create = task_commit(
        &task_id,
        &actor_id,
        "verified-create",
        '6',
        vec![submitted(&task_id, &actor_id)],
        Vec::new(),
    );
    store.commit_task(&create).unwrap();

    let snapshot_json: String = store
        .connection()
        .query_row(
            "SELECT snapshot_json FROM tasks WHERE task_id = ?1",
            params![task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let mut snapshot: serde_json::Value = serde_json::from_str(&snapshot_json).unwrap();
    snapshot["state"] = serde_json::Value::String("queued".to_string());
    store
        .connection()
        .execute(
            "UPDATE tasks SET snapshot_json = ?2 WHERE task_id = ?1",
            params![task_id.as_str(), serde_json::to_string(&snapshot).unwrap()],
        )
        .unwrap();

    assert!(matches!(
        store.load_task(&task_id),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        store.commit_task(&create),
        Err(StoreError::Corrupt { .. })
    ));

    let queued = envelope(
        &task_id,
        &actor_id,
        2,
        TaskEvent::TaskQueued {
            run_id: RunId::new(),
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
    );
    assert!(matches!(
        store.commit_task(&task_commit(
            &task_id,
            &actor_id,
            "verified-append",
            '7',
            vec![queued],
            Vec::new(),
        )),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(table_count(&store, "task_events"), 1);
    assert_eq!(table_count(&store, "command_receipts"), 1);
}
