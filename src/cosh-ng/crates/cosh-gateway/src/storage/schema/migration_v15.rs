use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 15,
    checksum: "cosh-gateway-task-snapshot-switch-v15-20260826",
    sql: r#"
CREATE TABLE task_snapshot_switches (
    actor_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    command_digest TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    snapshot_id TEXT NOT NULL,
    preview_digest TEXT NOT NULL,
    expected_revision INTEGER NOT NULL,
    recovery_snapshot_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('intent', 'recovery_created', 'switch_started', 'succeeded', 'unknown', 'failed')),
    result_json TEXT,
    reason TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(actor_id, idempotency_key)
);

CREATE INDEX task_snapshot_switches_recovery
    ON task_snapshot_switches(state, updated_at_ms);
"#,
};
