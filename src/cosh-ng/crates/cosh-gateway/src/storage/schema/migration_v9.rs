use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 9,
    checksum: "cosh-gateway-runtime-input-v9-20260816-fenced-private-response",
    sql: r#"
CREATE TABLE runtime_input_requests (
    request_id TEXT PRIMARY KEY NOT NULL,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    binding_id TEXT NOT NULL REFERENCES runtime_bindings(binding_id) ON DELETE RESTRICT,
    runtime_instance_id TEXT NOT NULL,
    runtime_generation INTEGER NOT NULL CHECK (runtime_generation > 0),
    runtime_sequence INTEGER NOT NULL CHECK (runtime_sequence > 0),
    lease_generation INTEGER NOT NULL CHECK (lease_generation > 0),
    lease_revision INTEGER NOT NULL CHECK (lease_revision > 0),
    request_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'resolved', 'expired', 'cancelled')),
    response_digest TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CHECK ((state = 'resolved') = (response_digest IS NOT NULL))
) STRICT;

CREATE INDEX runtime_input_requests_run_state
    ON runtime_input_requests(run_id, state, updated_at_ms);

CREATE TABLE runtime_input_dispatches (
    request_id TEXT PRIMARY KEY NOT NULL
        REFERENCES runtime_input_requests(request_id) ON DELETE RESTRICT,
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    response_json TEXT NOT NULL,
    response_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('prepared', 'started', 'delivered', 'unknown')),
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
) STRICT;

CREATE INDEX runtime_input_dispatches_recovery
    ON runtime_input_dispatches(state, updated_at_ms);
"#,
};
