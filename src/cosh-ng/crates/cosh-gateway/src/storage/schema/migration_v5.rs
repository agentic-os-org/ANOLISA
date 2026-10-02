use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 5,
    checksum: "cosh-gateway-legacy-runtime-recovery-v5-20260816-admin-receipt",
    sql: r#"
CREATE TABLE legacy_runtime_start_recoveries (
    task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
    run_id TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason = 'missing_runtime_start_intent'),
    state TEXT NOT NULL CHECK (state IN ('pending', 'settled')),
    detected_at_ms INTEGER NOT NULL CHECK (detected_at_ms >= 0),
    settled_revision INTEGER CHECK (settled_revision > 0),
    settled_at_ms INTEGER CHECK (settled_at_ms >= detected_at_ms),
    settlement_digest TEXT,
    settlement_event_ids_json TEXT,
    CHECK ((state = 'settled') = (settled_revision IS NOT NULL)),
    CHECK ((state = 'settled') = (settled_at_ms IS NOT NULL)),
    CHECK ((state = 'settled') = (settlement_digest IS NOT NULL)),
    CHECK ((state = 'settled') = (settlement_event_ids_json IS NOT NULL))
) STRICT;

INSERT INTO legacy_runtime_start_recoveries(
    task_id, run_id, reason, state, detected_at_ms
)
SELECT
    t.task_id,
    json_extract(t.snapshot_json, '$.active_run_id'),
    'missing_runtime_start_intent',
    'pending',
    CAST(unixepoch('subsec') * 1000 AS INTEGER)
FROM tasks t
WHERE t.state = 'queued'
  AND json_type(t.snapshot_json, '$.active_run_id') = 'text'
  AND NOT EXISTS (
      SELECT 1
      FROM outbox o
      WHERE o.task_id = t.task_id
        AND o.delivery_kind = 'runtime_start'
        AND json_extract(o.payload_json, '$.run_id') =
            json_extract(t.snapshot_json, '$.active_run_id')
  );
"#,
};
