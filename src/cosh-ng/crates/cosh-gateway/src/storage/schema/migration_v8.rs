use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 8,
    checksum: "cosh-gateway-brokered-result-v8-20260816-typed-atomic",
    sql: r#"
ALTER TABLE executions ADD COLUMN typed_result_state TEXT CHECK (
    typed_result_state IN ('not_applicable', 'available', 'legacy_unavailable')
);

UPDATE executions
SET typed_result_state = CASE
    WHEN state = 'succeeded' THEN 'legacy_unavailable'
    ELSE 'not_applicable'
END;

CREATE TABLE brokered_execution_results (
    execution_id TEXT PRIMARY KEY NOT NULL
        REFERENCES executions(execution_id) ON DELETE RESTRICT,
    request_id TEXT NOT NULL
        REFERENCES brokered_requests(request_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    result_json TEXT NOT NULL,
    result_digest TEXT NOT NULL,
    operation_json TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    target_identity_digest TEXT NOT NULL,
    runtime_fence_json TEXT NOT NULL,
    committed_at_ms INTEGER NOT NULL CHECK (committed_at_ms >= 0)
) STRICT;

CREATE INDEX brokered_execution_results_request
    ON brokered_execution_results(request_id, committed_at_ms);
"#,
};
