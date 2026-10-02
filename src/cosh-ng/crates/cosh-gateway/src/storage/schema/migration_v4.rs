use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 4,
    checksum: "cosh-gateway-provider-permission-schema-v4-20260816",
    sql: r#"
ALTER TABLE approvals ADD COLUMN permission_ref_json TEXT;

CREATE TABLE provider_permission_dispatches (
    approval_id TEXT PRIMARY KEY NOT NULL
        REFERENCES approvals(approval_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    permission_ref_json TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('allow_once', 'deny')),
    state TEXT NOT NULL CHECK (
        state IN ('prepared', 'started', 'delivered', 'unknown')
    ),
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
) STRICT;

CREATE INDEX provider_permission_dispatches_recovery
    ON provider_permission_dispatches(state, updated_at_ms);
"#,
};
