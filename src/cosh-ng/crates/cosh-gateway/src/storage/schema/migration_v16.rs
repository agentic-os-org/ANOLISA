use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 16,
    checksum: "cosh-gateway-snapshot-recovery-proof-v16-20260910",
    sql: r#"
ALTER TABLE task_snapshot_switches ADD COLUMN recovery_proven INTEGER NOT NULL
    DEFAULT 0 CHECK(recovery_proven IN (0, 1));

-- These v15 states can only be reached after recovery creation was proven.
-- A failed v15 operation may have failed before creation, so it proves nothing.
UPDATE task_snapshot_switches SET recovery_proven=1
    WHERE state IN ('recovery_created', 'switch_started', 'succeeded', 'unknown');
"#,
};
