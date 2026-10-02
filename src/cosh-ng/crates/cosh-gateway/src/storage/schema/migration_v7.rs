use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 7,
    checksum: "cosh-gateway-brokered-callback-dispatch-v7-20260816-fenced",
    sql: r#"
CREATE TABLE brokered_runtime_dispatches (
    request_id TEXT NOT NULL
        REFERENCES brokered_requests(request_id) ON DELETE RESTRICT,
    dispatch_kind TEXT NOT NULL CHECK (dispatch_kind IN ('acknowledgement', 'result')),
    actor_id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    brokered_ref_json TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('approval_pending', 'approval_denied', 'execution')),
    source_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('prepared', 'started', 'delivered', 'unknown')),
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms),
    PRIMARY KEY(request_id, dispatch_kind),
    CHECK ((dispatch_kind = 'acknowledgement') = (source_kind = 'approval_pending')),
    CHECK ((dispatch_kind = 'result') = (source_kind IN ('approval_denied', 'execution')))
) STRICT;

CREATE INDEX brokered_runtime_dispatches_recovery
    ON brokered_runtime_dispatches(state, updated_at_ms);
"#,
};
