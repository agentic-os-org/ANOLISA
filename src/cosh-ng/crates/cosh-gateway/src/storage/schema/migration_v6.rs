use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 6,
    checksum: "cosh-gateway-brokered-execution-v6-20260816-fenced-claim-audit",
    sql: r#"
ALTER TABLE approvals ADD COLUMN target_identity_digest TEXT;
ALTER TABLE approvals ADD COLUMN runtime_fence_json TEXT;

ALTER TABLE permits ADD COLUMN target_identity_digest TEXT;
ALTER TABLE permits ADD COLUMN runtime_fence_json TEXT;

-- Pre-v6 permits cannot prove immutable target or Runtime authority. They are
-- retained for audit but must never remain executable after upgrade.
UPDATE permits SET state = 'revoked' WHERE state = 'issued';

ALTER TABLE executions ADD COLUMN target_identity_digest TEXT;
ALTER TABLE executions ADD COLUMN runtime_fence_json TEXT;
ALTER TABLE executions ADD COLUMN broker_state TEXT CHECK (
    broker_state IS NULL OR broker_state IN ('planned', 'claimed', 'started', 'known_no_effect')
);
ALTER TABLE executions ADD COLUMN claimed_at_ms INTEGER CHECK (claimed_at_ms >= 0);
ALTER TABLE executions ADD COLUMN start_audit_proof_digest TEXT;

CREATE TABLE brokered_requests (
    request_id TEXT PRIMARY KEY NOT NULL,
    approval_id TEXT UNIQUE REFERENCES approvals(approval_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    request_json TEXT NOT NULL,
    operation_json TEXT NOT NULL,
    typed_operation_digest TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    target_identity_digest TEXT NOT NULL,
    runtime_fence_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0)
) STRICT;

CREATE TABLE security_audit_proofs (
    execution_id TEXT PRIMARY KEY NOT NULL
        REFERENCES executions(execution_id) ON DELETE RESTRICT,
    proof_digest TEXT NOT NULL,
    durability TEXT NOT NULL CHECK (durability = 'security_boundary'),
    persisted_at_ms INTEGER NOT NULL CHECK (persisted_at_ms >= 0)
) STRICT;

CREATE INDEX brokered_requests_task_run
    ON brokered_requests(task_id, run_id, created_at_ms);
CREATE INDEX executions_broker_recovery
    ON executions(broker_state, updated_at_ms);
"#,
};
