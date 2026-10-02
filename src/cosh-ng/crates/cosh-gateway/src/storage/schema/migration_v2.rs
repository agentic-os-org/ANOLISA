use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 2,
    checksum: "cosh-gateway-ledger-schema-v2-20260814-fenced",
    sql: r#"
CREATE TABLE ledger_receipts (
    actor_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    command_digest TEXT NOT NULL,
    operation TEXT NOT NULL,
    result_json TEXT NOT NULL,
    committed_at_ms INTEGER NOT NULL CHECK (committed_at_ms >= 0),
    PRIMARY KEY(actor_id, idempotency_key)
) STRICT;

CREATE TABLE approvals (
    approval_id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    target_json TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'approved', 'denied', 'expired', 'cancelled')
    ),
    revision INTEGER NOT NULL CHECK (revision > 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0),
    decided_by_actor_id TEXT,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CHECK ((state IN ('approved', 'denied')) = (decided_by_actor_id IS NOT NULL))
) STRICT;

CREATE TABLE executions (
    execution_id TEXT PRIMARY KEY NOT NULL,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    target_json TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('planned', 'started', 'succeeded', 'failed', 'uncertain')
    ),
    revision INTEGER NOT NULL CHECK (revision > 0),
    started_at_ms INTEGER,
    completed_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CHECK ((state = 'planned') = (started_at_ms IS NULL)),
    CHECK ((state IN ('succeeded', 'failed', 'uncertain')) = (completed_at_ms IS NOT NULL))
) STRICT;

CREATE TABLE permits (
    permit_id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    approval_id TEXT REFERENCES approvals(approval_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    execution_id TEXT NOT NULL UNIQUE REFERENCES executions(execution_id) ON DELETE RESTRICT,
    target_json TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    policy_revision INTEGER NOT NULL CHECK (policy_revision >= 0),
    state TEXT NOT NULL CHECK (state IN ('issued', 'consumed', 'expired', 'revoked')),
    single_use INTEGER NOT NULL CHECK (single_use = 1),
    valid_until_ms INTEGER NOT NULL CHECK (valid_until_ms >= 0),
    consumed_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    CHECK ((state = 'consumed') = (consumed_at_ms IS NOT NULL))
) STRICT;

CREATE TABLE execution_receipts (
    execution_id TEXT PRIMARY KEY NOT NULL REFERENCES executions(execution_id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (state IN ('succeeded', 'failed')),
    receipt_digest TEXT NOT NULL,
    safe_detail TEXT,
    committed_at_ms INTEGER NOT NULL CHECK (committed_at_ms >= 0)
) STRICT;

CREATE TABLE runtime_bindings (
    binding_id TEXT PRIMARY KEY NOT NULL,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    runtime_instance_id TEXT NOT NULL,
    runtime_generation INTEGER NOT NULL CHECK (runtime_generation > 0),
    binding_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'closed', 'lost')),
    last_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_sequence >= 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    UNIQUE(run_id, runtime_generation)
) STRICT;

CREATE TABLE run_leases (
    run_id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    lease_owner TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;

CREATE INDEX approvals_pending ON approvals(state, expires_at_ms);
CREATE INDEX permits_issued ON permits(state, valid_until_ms);
CREATE INDEX executions_recovery ON executions(state, updated_at_ms);
CREATE INDEX runtime_bindings_run ON runtime_bindings(run_id, state);
CREATE INDEX run_leases_expiry ON run_leases(expires_at_ms);

CREATE TRIGGER command_receipts_reserve_idempotency_namespace
BEFORE INSERT ON command_receipts
WHEN EXISTS (
    SELECT 1 FROM ledger_receipts
    WHERE actor_id = NEW.actor_id AND idempotency_key = NEW.idempotency_key
)
BEGIN
    SELECT RAISE(ABORT, 'idempotency namespace conflict');
END;

CREATE TRIGGER ledger_receipts_reserve_idempotency_namespace
BEFORE INSERT ON ledger_receipts
WHEN EXISTS (
    SELECT 1 FROM command_receipts
    WHERE actor_id = NEW.actor_id AND idempotency_key = NEW.idempotency_key
)
BEGIN
    SELECT RAISE(ABORT, 'idempotency namespace conflict');
END;
"#,
};
