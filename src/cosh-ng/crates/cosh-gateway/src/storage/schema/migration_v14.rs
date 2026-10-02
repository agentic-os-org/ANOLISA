use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 14,
    checksum: "cosh-gateway-approval-checkpoint-barrier-v14-20260826",
    sql: r#"
CREATE TABLE approval_checkpoint_barriers (
    approval_id TEXT PRIMARY KEY REFERENCES approvals(approval_id),
    task_id TEXT NOT NULL REFERENCES tasks(task_id),
    run_id TEXT NOT NULL,
    checkpoint_id TEXT NOT NULL UNIQUE,
    policy TEXT NOT NULL CHECK(policy IN ('auto', 'on')),
    runtime_fence_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('intent', 'started', 'created', 'skipped', 'unknown', 'failed')),
    binding_json TEXT,
    evidence_json TEXT,
    reason_json TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX approval_checkpoint_barriers_recovery
    ON approval_checkpoint_barriers(state, updated_at_ms);
"#,
};
