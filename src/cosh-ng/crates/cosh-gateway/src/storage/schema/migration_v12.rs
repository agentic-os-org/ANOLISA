use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 12,
    checksum: "cosh-gateway-pre-runtime-baseline-v12-20260826",
    sql: r#"
CREATE TABLE pre_runtime_baselines (
    task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    baseline_id TEXT NOT NULL UNIQUE,
    policy TEXT NOT NULL CHECK (policy IN ('auto', 'on')),
    state TEXT NOT NULL CHECK (
        state IN ('started', 'created', 'skipped', 'unknown', 'failed')
    ),
    evidence_json TEXT,
    reason_json TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
) STRICT;

CREATE INDEX pre_runtime_baselines_run
    ON pre_runtime_baselines(run_id, state);
"#,
};
