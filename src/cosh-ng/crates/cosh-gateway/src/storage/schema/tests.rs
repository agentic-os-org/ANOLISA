use super::*;

#[test]
fn migration_is_repeatable_and_enables_all_tables() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate(&mut connection).unwrap();
    migrate(&mut connection).unwrap();

    let tables = connection
        .prepare(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        tables,
        [
            "approval_checkpoint_barriers",
            "approvals",
            "brokered_execution_results",
            "brokered_requests",
            "brokered_runtime_dispatches",
            "command_receipts",
            "execution_receipts",
            "executions",
            "gateway_identity",
            "ledger_receipts",
            "legacy_runtime_start_recoveries",
            "outbox",
            "permits",
            "pre_runtime_baselines",
            "provider_permission_dispatches",
            "run_leases",
            "runtime_bindings",
            "runtime_input_dispatches",
            "runtime_input_requests",
            "schema_migrations",
            "security_audit_proofs",
            "task_events",
            "task_snapshot_switches",
            "tasks"
        ]
    );
}

#[test]
fn existing_v1_database_migrates_without_rewriting_v1() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE schema_migrations (
                 version INTEGER PRIMARY KEY NOT NULL CHECK (version > 0),
                 checksum TEXT NOT NULL,
                 applied_at_ms INTEGER NOT NULL CHECK (applied_at_ms >= 0)
             ) STRICT;",
        )
        .unwrap();
    apply_migration(&mut connection, &MIGRATIONS[0]).unwrap();

    migrate(&mut connection).unwrap();

    let versions = connection
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| row.get::<_, u32>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        versions,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    );
    let v1_checksum: String = connection
        .query_row(
            "SELECT checksum FROM schema_migrations WHERE version=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(v1_checksum, MIGRATIONS[0].checksum);
}

#[test]
fn existing_v8_database_adds_private_runtime_input_tables() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 8).unwrap();
    let before: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type='table' AND name LIKE 'runtime_input_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, 0);

    migrate(&mut connection).unwrap();

    let version: u32 = connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 16);
    let tables = connection
        .prepare(
            "SELECT name FROM sqlite_schema
             WHERE type='table' AND name LIKE 'runtime_input_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        tables,
        ["runtime_input_dispatches", "runtime_input_requests"]
    );
}

#[test]
fn existing_v10_database_migrates_provider_dispatch_states_without_regranting_authority() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 10).unwrap();

    let actor = "actor-00000000-0000-0000-0000-000000000001";
    let task = "task-00000000-0000-0000-0000-000000000001";
    let run = "run-00000000-0000-0000-0000-000000000001";
    let approval = "approval-00000000-0000-0000-0000-000000000001";
    let request = "request-00000000-0000-0000-0000-000000000001";
    connection
        .execute(
            "INSERT INTO tasks(
                 task_id, owner_actor_id, target_ref, revision, state,
                 snapshot_json, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'test', 1, 'succeeded', '{}', 1, 1)",
            params![task, actor],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO approvals(
                 approval_id, request_id, actor_id, task_id, run_id, target_json,
                 operation_digest, input_digest, state, revision, expires_at_ms,
                 created_at_ms, updated_at_ms, permission_ref_json)
             VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6, ?6, 'pending', 1, 100, 1, 1, '{}')",
            params![approval, request, actor, task, run, "0".repeat(64)],
        )
        .unwrap();
    for (suffix, state) in [("-started", "started"), ("-delivered", "delivered")] {
        let approval_id = format!("{approval}{suffix}");
        connection
            .execute(
                "INSERT INTO approvals(
                     approval_id, request_id, actor_id, task_id, run_id, target_json,
                     operation_digest, input_digest, state, revision, expires_at_ms,
                     created_at_ms, updated_at_ms, permission_ref_json)
                 SELECT ?1, request_id || ?2, actor_id, task_id, run_id, target_json,
                        operation_digest, input_digest, state, revision, expires_at_ms,
                        created_at_ms, updated_at_ms, permission_ref_json
                 FROM approvals WHERE approval_id=?3",
                params![approval_id, suffix, approval],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO provider_permission_dispatches(
                     approval_id, actor_id, task_id, run_id, permission_ref_json,
                     decision, state, revision, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, '{}', 'allow_once', ?5, 2, 1, 1)",
                params![approval_id, actor, task, run, state],
            )
            .unwrap();
    }

    migrate(&mut connection).unwrap();

    let states = connection
        .prepare("SELECT state FROM provider_permission_dispatches ORDER BY approval_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(states, ["written", "write_started"]);
}

#[test]
fn existing_v9_database_adds_provider_recovery_binding() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 9).unwrap();

    let before = columns(&connection, "brokered_requests");
    assert!(!before.iter().any(|column| column == "provider_binding"));

    migrate(&mut connection).unwrap();

    let after = columns(&connection, "brokered_requests");
    assert!(after.iter().any(|column| column == "provider_binding"));
}

#[test]
fn existing_v12_database_adds_nullable_pre_runtime_provider_binding() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 12).unwrap();
    assert!(!columns(&connection, "pre_runtime_baselines")
        .iter()
        .any(|column| column == "binding_json"));

    migrate(&mut connection).unwrap();

    assert!(columns(&connection, "pre_runtime_baselines")
        .iter()
        .any(|column| column == "binding_json"));
}

fn columns(connection: &Connection, table: &str) -> Vec<String> {
    connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn newer_schema_fails_closed() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations(version, checksum, applied_at_ms)
             VALUES (?1, 'future', 0)",
            [CURRENT_SCHEMA_VERSION + 1],
        )
        .unwrap();

    assert!(matches!(
        migrate(&mut connection),
        Err(StoreError::NewerSchema { .. })
    ));
}

#[test]
fn v5_provider_approval_migrates_without_manufacturing_brokered_authority() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 5).unwrap();
    connection
        .execute_batch(
            "INSERT INTO tasks(
                 task_id, owner_actor_id, target_ref, revision, state,
                 snapshot_json, created_at_ms, updated_at_ms)
             VALUES ('task', 'actor', '{}', 1, 'running', '{}', 1, 1);
             INSERT INTO approvals(
                 approval_id, request_id, actor_id, task_id, run_id, target_json,
                 operation_digest, input_digest, state, revision, expires_at_ms,
                 created_at_ms, updated_at_ms, permission_ref_json)
             VALUES (
                 'approval', 'request', 'actor', 'task', 'run', '{}',
                 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                 'pending', 1, 100, 1, 1, '{\"runtime_generation\":1}'
             );",
        )
        .unwrap();

    migrate(&mut connection).unwrap();

    let row = connection
        .query_row(
            "SELECT permission_ref_json, target_identity_digest, runtime_fence_json
             FROM approvals WHERE approval_id='approval'",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0.as_deref(), Some("{\"runtime_generation\":1}"));
    assert_eq!(row.1, None);
    assert_eq!(row.2, None);
}

#[test]
fn checksum_mismatch_fails_closed() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection
        .execute(
            "UPDATE schema_migrations SET checksum = 'changed' WHERE version = 1",
            [],
        )
        .unwrap();
    assert!(matches!(
        migrate(&mut connection),
        Err(StoreError::MigrationChecksum { version: 1 })
    ));
}

#[test]
fn migration_history_must_be_contiguous() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 3).unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version = 2", [])
        .unwrap();

    let error = migrate(&mut connection).unwrap_err();
    assert!(matches!(
        error,
        StoreError::Corrupt { message }
            if message.contains("not contiguous")
    ));
}

#[test]
fn migration_fk_failure_rolls_back_version() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 1).unwrap();
    let invalid_migration = Migration {
        version: 2,
        checksum: "test-invalid-foreign-key",
        sql: r#"
PRAGMA defer_foreign_keys = ON;
INSERT INTO outbox(
delivery_id, task_id, event_id, delivery_kind, payload_json, state,
attempt, next_attempt_at_ms, lease_owner, lease_expires_at_ms,
created_at_ms, delivered_at_ms
) VALUES (
'orphan-delivery', 'missing-task', 'missing-event', 'runtime_start', '{}',
'pending', 0, 0, NULL, NULL, 0, NULL
);
"#,
    };

    let error = apply_migration(&mut connection, &invalid_migration).unwrap_err();
    assert!(matches!(
        error,
        StoreError::Corrupt { message }
            if message.contains("foreign_key_check")
    ));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE delivery_id = 'orphan-delivery'",
                [],
                |row| row.get::<_, u32>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 2",
                [],
                |row| row.get::<_, u32>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn v15_migration_preserves_only_proven_recovery_states() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    migrate_to_for_test(&mut connection, 15).unwrap();
    connection.execute(
        "INSERT INTO tasks(task_id,owner_actor_id,target_ref,revision,state,snapshot_json,created_at_ms,updated_at_ms)
         VALUES ('task','actor','{}',1,'cancelled','{}',1,1)", [],
    ).unwrap();
    let states = [
        "intent",
        "recovery_created",
        "switch_started",
        "succeeded",
        "unknown",
        "failed",
    ];
    for state in states {
        connection.execute(
            "INSERT INTO task_snapshot_switches(actor_id,idempotency_key,command_digest,task_id,snapshot_id,
             preview_digest,expected_revision,recovery_snapshot_id,state,created_at_ms,updated_at_ms)
             VALUES ('actor',?1,'digest','task','snapshot','preview',1,?1,?1,1,1)",
            params![state],
        ).unwrap();
    }
    migrate(&mut connection).unwrap();
    migrate(&mut connection).unwrap();
    for state in states {
        let proven: bool = connection
            .query_row(
                "SELECT recovery_proven FROM task_snapshot_switches WHERE state=?1",
                params![state],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(proven, !matches!(state, "intent" | "failed"), "{state}");
    }
}
