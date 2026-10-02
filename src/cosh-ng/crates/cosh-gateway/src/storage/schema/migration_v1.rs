use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 1,
    checksum: "cosh-gateway-task-schema-v1-20260813-causation-nullable",
    sql: r#"
CREATE TABLE tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    owner_actor_id TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
) STRICT;

CREATE TABLE task_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    event_type TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    payload_json TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms >= 0),
    causation_id TEXT,
    correlation_id TEXT,
    UNIQUE(task_id, revision)
) STRICT;

CREATE TABLE command_receipts (
    actor_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    command_digest TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    task_revision INTEGER NOT NULL CHECK (task_revision >= 0),
    receipt_json TEXT NOT NULL,
    committed_at_ms INTEGER NOT NULL CHECK (committed_at_ms >= 0),
    PRIMARY KEY(actor_id, idempotency_key)
) STRICT;

CREATE TABLE outbox (
    delivery_id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    event_id TEXT NOT NULL REFERENCES task_events(event_id) ON DELETE RESTRICT,
    delivery_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'leased', 'delivered', 'dead_letter')),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    next_attempt_at_ms INTEGER NOT NULL CHECK (next_attempt_at_ms >= 0),
    lease_owner TEXT,
    lease_expires_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    delivered_at_ms INTEGER,
    CHECK ((state = 'leased') = (lease_owner IS NOT NULL)),
    CHECK ((state = 'leased') = (lease_expires_at_ms IS NOT NULL))
) STRICT;

CREATE INDEX task_events_task_revision
    ON task_events(task_id, revision);
CREATE INDEX outbox_ready
    ON outbox(state, next_attempt_at_ms, created_at_ms);
"#,
};
