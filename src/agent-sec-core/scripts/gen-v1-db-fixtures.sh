#!/usr/bin/env bash
#
# Generates the frozen v1 database fixtures and their expected projections.
#
# Run this once, commit the output, and the Rust suite can then catch schema
# drift and migration regressions with no Python environment present. The live
# differential matrix (`make test-db-compat`) needs v1; these fixtures do not.
#
# Two things here are deliberate and should not be "cleaned up":
#
# 1. **Raw DDL for the legacy schemas.** No current code can produce a revision-1
#    or revision-2 database: v1's own `ensure_schema` always converges to 3. The
#    only way to obtain a historical database is to forge it, so the pre-upgrade
#    fixtures are written with explicit SQL that reproduces the schema as it
#    shipped. The column lists come from
#    `SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS`.
# 2. **v1 produces the oracle.** After forging each fixture, a *copy* is upgraded
#    by v1 and v1 dumps the resulting projection. That JSON is the expected
#    output; v2 must reproduce it. Generating the oracle from v2 would make the
#    test tautological.
#
# Usage: scripts/gen-v1-db-fixtures.sh

set -euo pipefail

CORE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE_DIR="$CORE_DIR/v2/crates/asc-persistence-sqlite/tests/fixtures/v1"
V1_PROBE="$CORE_DIR/tests/compat/v1_db_probe.py"

export TZ=UTC
export LC_ALL=C

mkdir -p "$FIXTURE_DIR"

log() { printf '%s\n' "$*"; }

# Runs the v1 probe against an explicit database path.
v1() {
  ( cd "$CORE_DIR" && uv run --project agent-sec-cli python "$V1_PROBE" "$@" )
}

# Upgrades a copy of a fixture with v1 and writes the resulting projection.
#
# Usage: freeze_projection <fixture-name> <stream>
freeze_projection() {
  local name="$1" stream="$2"
  local work
  work="$(mktemp -d)"
  cp "$FIXTURE_DIR/$name.db" "$work/upgraded.db"

  v1 --db "$work/upgraded.db" --stream "$stream" ensure-schema > /dev/null
  v1 --db "$work/upgraded.db" --stream "$stream" dump-schema \
    | python3 -m json.tool --sort-keys > "$FIXTURE_DIR/$name.expected.json"
  # Row-level oracles only for the security stream: `read-events` is a
  # security-stream command, and the observability read path is covered by the
  # live matrix instead.
  if [ "$stream" = security ]; then
    v1 --db "$work/upgraded.db" --stream "$stream" read-events \
      | python3 -m json.tool --sort-keys > "$FIXTURE_DIR/$name.expected-rows.json"
  fi

  rm -rf "$work"
  strip_sidecars "$name"
  log "  froze $name.expected.json"
}

# Removes the files SQLite and the maintenance gate leave beside a database.
#
# `-wal` and `-shm` are checkpoint state, and `.maintenance*` is the retention
# gate's timestamp and lock. None of them belong in a committed fixture: they are
# regenerated on demand, and a stale `-wal` would make the fixture's content
# depend on whether it was checkpointed before commit.
strip_sidecars() {
  local name="$1"
  rm -f "$FIXTURE_DIR/$name.db-wal" \
        "$FIXTURE_DIR/$name.db-shm" \
        "$FIXTURE_DIR/$name.db.maintenance" \
        "$FIXTURE_DIR/$name.db.maintenance.lock"
}

# --------------------------------------------------------------------------
# revision 1: no correlation columns, no verdict
# --------------------------------------------------------------------------

log "generating security_events_v1.db"
rm -f "$FIXTURE_DIR/security_events_v1.db"
sqlite3 "$FIXTURE_DIR/security_events_v1.db" > /dev/null <<'SQL'
PRAGMA journal_mode=WAL;
CREATE TABLE security_events (
  event_id TEXT NOT NULL PRIMARY KEY,
  event_type TEXT NOT NULL,
  category TEXT NOT NULL,
  result TEXT NOT NULL DEFAULT 'succeeded',
  timestamp TEXT NOT NULL,
  timestamp_epoch FLOAT NOT NULL,
  trace_id TEXT NOT NULL DEFAULT '',
  pid INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  session_id TEXT,
  details TEXT NOT NULL
);
CREATE INDEX idx_timestamp_epoch ON security_events (timestamp_epoch);
CREATE INDEX idx_event_type ON security_events (event_type);
CREATE INDEX idx_trace_id ON security_events (trace_id);
INSERT INTO security_events VALUES
  ('r1-a','code_scan','code_scan','succeeded','2026-01-02T03:04:05+00:00',
   1767322445.0,'t-1',11,501,'s-1','{"result":{"verdict":"deny"}}'),
  ('r1-b','harden','hardening','failed','2026-01-02T04:00:00+00:00',
   1767325200.0,'',12,0,NULL,'{"result":{"passed":8,"total":10}}');
PRAGMA user_version=1;
SQL
freeze_projection security_events_v1 security

# --------------------------------------------------------------------------
# revision 2: correlation columns present, verdict absent
# --------------------------------------------------------------------------

# The shared revision-2 DDL, reused by the plain and the mixed-details fixture.
revision_two_ddl() {
  cat <<'SQL'
PRAGMA journal_mode=WAL;
CREATE TABLE security_events (
  event_id TEXT NOT NULL PRIMARY KEY,
  event_type TEXT NOT NULL,
  category TEXT NOT NULL,
  result TEXT NOT NULL DEFAULT 'succeeded',
  timestamp TEXT NOT NULL,
  timestamp_epoch FLOAT NOT NULL,
  trace_id TEXT NOT NULL DEFAULT '',
  pid INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  session_id TEXT,
  run_id TEXT,
  call_id TEXT,
  tool_call_id TEXT,
  details TEXT NOT NULL
);
CREATE INDEX idx_timestamp_epoch ON security_events (timestamp_epoch);
CREATE INDEX idx_event_type ON security_events (event_type);
CREATE INDEX idx_trace_id ON security_events (trace_id);
CREATE INDEX idx_session_id_timestamp_epoch
  ON security_events (session_id, timestamp_epoch);
CREATE INDEX idx_run_id_timestamp_epoch
  ON security_events (run_id, timestamp_epoch);
CREATE INDEX idx_session_run_timestamp_epoch
  ON security_events (session_id, run_id, timestamp_epoch);
SQL
}

log "generating security_events_v2.db"
rm -f "$FIXTURE_DIR/security_events_v2.db"
{
  revision_two_ddl
  cat <<'SQL'
INSERT INTO security_events VALUES
  ('r2-a','code_scan','code_scan','succeeded','2026-01-02T03:04:05+00:00',
   1767322445.0,'t-1',11,501,'s-1','r-1','c-1','tc-1',
   '{"result":{"verdict":"deny"}}'),
  ('r2-b','code_scan','code_scan','succeeded','2026-01-02T03:05:05+00:00',
   1767322505.0,'t-2',12,501,'s-1','r-2',NULL,NULL,
   '{"result":{"verdict":"allow"}}');
PRAGMA user_version=2;
SQL
} | sqlite3 "$FIXTURE_DIR/security_events_v2.db" > /dev/null
freeze_projection security_events_v2 security

# --------------------------------------------------------------------------
# revision 2 with adversarial `details`: the backfill must skip, not stall
# --------------------------------------------------------------------------

log "generating security_events_v2_mixed.db"
rm -f "$FIXTURE_DIR/security_events_v2_mixed.db"
{
  revision_two_ddl
  # Five shapes, in this order: extractable verdict, object without a verdict
  # field, JSON array, malformed JSON, empty string. Only the first can be
  # backfilled; the other four must leave `verdict` NULL *and* still let the
  # cursor advance. A backfill that retried unbackfillable rows would loop here.
  cat <<'SQL'
INSERT INTO security_events VALUES
  ('mix-1','code_scan','code_scan','succeeded','2026-01-02T00:00:01+00:00',
   1767312001.0,'',1,0,'s-1','r-1',NULL,NULL,
   '{"result":{"verdict":"deny"}}'),
  ('mix-2','code_scan','code_scan','succeeded','2026-01-02T00:00:02+00:00',
   1767312002.0,'',1,0,'s-1','r-1',NULL,NULL,
   '{"result":{"risk":3}}'),
  ('mix-3','code_scan','code_scan','succeeded','2026-01-02T00:00:03+00:00',
   1767312003.0,'',1,0,'s-1','r-1',NULL,NULL,
   '[1,2,3]'),
  ('mix-4','code_scan','code_scan','succeeded','2026-01-02T00:00:04+00:00',
   1767312004.0,'',1,0,'s-1','r-1',NULL,NULL,
   '{not json'),
  ('mix-5','code_scan','code_scan','succeeded','2026-01-02T00:00:05+00:00',
   1767312005.0,'',1,0,'s-1','r-1',NULL,NULL,
   '');
PRAGMA user_version=2;
SQL
} | sqlite3 "$FIXTURE_DIR/security_events_v2_mixed.db" > /dev/null
freeze_projection security_events_v2_mixed security

# --------------------------------------------------------------------------
# a future revision: the downgrade guard must leave it alone
# --------------------------------------------------------------------------

log "generating security_events_v4_future.db"
rm -f "$FIXTURE_DIR/security_events_v4_future.db"
sqlite3 "$FIXTURE_DIR/security_events_v4_future.db" > /dev/null <<'SQL'
PRAGMA journal_mode=WAL;
-- The current schema plus a column no released version knows about. A writer
-- from the future wrote this; neither v1 nor v2 may modify it.
CREATE TABLE security_events (
  event_id TEXT NOT NULL PRIMARY KEY,
  event_type TEXT NOT NULL,
  category TEXT NOT NULL,
  result TEXT NOT NULL DEFAULT 'succeeded',
  timestamp TEXT NOT NULL,
  timestamp_epoch FLOAT NOT NULL,
  trace_id TEXT NOT NULL DEFAULT '',
  pid INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  session_id TEXT,
  run_id TEXT,
  call_id TEXT,
  tool_call_id TEXT,
  verdict TEXT,
  details TEXT NOT NULL,
  future_column TEXT
);
INSERT INTO security_events VALUES
  ('future-a','code_scan','code_scan','succeeded','2026-01-02T03:04:05+00:00',
   1767322445.0,'',1,0,'s-1','r-1',NULL,NULL,'deny',
   '{"result":{"verdict":"deny"}}','tomorrow');
PRAGMA user_version=4;
SQL
freeze_projection security_events_v4_future security

# --------------------------------------------------------------------------
# observability at its only revision
# --------------------------------------------------------------------------

log "generating observability_v1.db"
rm -f "$FIXTURE_DIR/observability_v1.db"
OBS_WORK="$(mktemp -d)"
cat > "$OBS_WORK/records.jsonl" <<'EOF'
{"hook":"after_tool_call","observedAt":"2026-01-02T03:04:05Z","metadata":{"sessionId":"s-1","runId":"r-1","toolCallId":"tc-1","callId":"c-1"},"metrics":{"duration_ms":12,"exit_code":0}}
{"hook":"after_agent_run","observedAt":"2026-01-02T03:10:00Z","metadata":{"sessionId":"s-1","runId":"r-1"},"metrics":{"duration_ms":900}}
EOF
# Written by v1 itself: this stream has never changed revision, so there is no
# historical schema to forge and the real writer is the better source.
v1 --db "$FIXTURE_DIR/observability_v1.db" --stream observability \
  write-events --input "$OBS_WORK/records.jsonl" > /dev/null
rm -rf "$OBS_WORK"
freeze_projection observability_v1 observability

# --------------------------------------------------------------------------
# provenance
# --------------------------------------------------------------------------

COMMIT="$(git -C "$CORE_DIR" rev-parse HEAD)"
V1_VERSION="$( cd "$CORE_DIR" && uv run --project agent-sec-cli python -c \
  'import importlib.metadata as m; print(m.version("agent-sec-cli"))' )"

cat > "$FIXTURE_DIR/README.md" <<EOF
# Frozen v1 database fixtures

Generated by \`scripts/gen-v1-db-fixtures.sh\`. **Do not edit by hand** — rerun the
generator instead, and review the resulting diff.

These exist so \`cargo test\` can catch schema drift and migration regressions with
no Python environment present. The live differential matrix
(\`make test-db-compat\`) still needs v1; this does not.

## Provenance

| | |
|---|---|
| generated from commit | \`$COMMIT\` |
| \`agent-sec-cli\` version | \`$V1_VERSION\` |
| generator | \`scripts/gen-v1-db-fixtures.sh\` |
| command | \`bash scripts/gen-v1-db-fixtures.sh\` |

## Contents

| File | What it pins |
|---|---|
| \`security_events_v1.db\` | Revision 1: no \`run_id\`/\`call_id\`/\`tool_call_id\`/\`verdict\`. Exercises generic column convergence. |
| \`security_events_v2.db\` | Revision 2: correlation columns present, \`verdict\` absent. Exercises the verdict backfill. |
| \`security_events_v2_mixed.db\` | Revision 2 with five \`details\` shapes — extractable verdict, object without \`verdict\`, JSON array, malformed JSON, empty string. Only the first is backfillable; the rest must be skipped **and** must not stall the batch cursor. |
| \`security_events_v4_future.db\` | \`user_version=4\`, i.e. written by a newer release. Both versions must warn and leave it untouched. |
| \`observability_v1.db\` | The observability stream at its only revision. |
| \`*.expected.json\` | The structural projection **after** v1 upgraded a copy. v2 must reproduce it. |
| \`*.expected-rows.json\` | The rows v1 reads back after the upgrade, where the stream supports it. |

The revision-1 and revision-2 databases are written with raw DDL on purpose: no
released code can produce them, because v1's own \`ensure_schema\` always converges
to the current revision. The column lists come from
\`SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS\`.

The expected projections are produced **by v1**, not by v2 — generating them from
v2 would make the test tautological.
EOF

log ""
log "wrote $(ls -1 "$FIXTURE_DIR" | wc -l | tr -d ' ') files to $FIXTURE_DIR"
