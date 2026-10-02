use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 11,
    checksum: "cosh-gateway-provider-dispatch-v11-20260825-write-semantics",
    sql: r#"
CREATE TABLE provider_permission_dispatches_v11 (
    approval_id TEXT PRIMARY KEY NOT NULL
        REFERENCES approvals(approval_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    permission_ref_json TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('allow_once', 'deny')),
    state TEXT NOT NULL CHECK (
        state IN ('prepared', 'write_started', 'written', 'abandoned', 'unknown')
    ),
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
) STRICT;

INSERT INTO provider_permission_dispatches_v11(
    approval_id, actor_id, task_id, run_id, permission_ref_json,
    decision, state, revision, created_at_ms, updated_at_ms)
SELECT approval_id, actor_id, task_id, run_id, permission_ref_json,
       decision,
       CASE state
           WHEN 'started' THEN 'write_started'
           WHEN 'delivered' THEN 'written'
           ELSE state
       END,
       revision, created_at_ms, updated_at_ms
FROM provider_permission_dispatches;

DROP TABLE provider_permission_dispatches;
ALTER TABLE provider_permission_dispatches_v11 RENAME TO provider_permission_dispatches;

CREATE INDEX provider_permission_dispatches_recovery
    ON provider_permission_dispatches(state, updated_at_ms);
"#,
};
