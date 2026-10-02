# ADR-009: Unified Audit Events, Single-writer Segments, and the Frozen SLS Contract

Date: 2026-07-22

Related documents: [Design](../design/audit-log.md) and
[Implementation Spec](../spec/audit-log-spec.md)

## Context

Three existing record types cannot substitute for one another. `cosh-platform::audit::LogEntry`
only represents PDP policy decisions, `cosh-core` SLS metrics only represent turn aggregates, and
most `cosh-shell` approval, activity, and details state exists only in process memory. The current
single `audit.log` also lets multiple short-lived processes append and rotate by rename, which
cannot represent complete Core and Shell lifecycles reliably and introduces cross-process rotation
races.

SLS already has a fixed production parsing contract: `/var/log/anolisa/sls/ops/cosh.jsonl`, 32
fields, field types and calculations, turn-finalization write timing, `COSH_SLS_LOG_PATH`, and
non-fatal write failures. Audit work must not change that contract.

## Decision

### Use a unified `AuditEventV1`

- Use a versioned JSONL envelope per event with fixed `schema = cosh.audit.event`,
  `schema_version = 1`, globally unique `event_id`, `event_type`, UTC timestamps, segment sequence,
  component, identity, actor, outcome, subject, typed data, and redaction metadata.
- Session, run, turn, request, tool-use, and command IDs represent distinct lifecycles and are not
  reused as one another.
- Version 1 permits only optional field additions, new event types, and unknown-value fallbacks.
  Renaming fields, changing units, or narrowing accepted values requires version 2.
- UI activity/details remain projections rather than persisted schema. PTY `events.jsonl` remains
  evidence and is not imported into version 1 audit.

### Use one unique, append-only segment per process

- Each `cosh-cli`, `cosh-core`, and `cosh-shell` process creates a unique file with a random
  `segment_id` and appends only to the active segment it created.
- An active segment uses `.jsonl.active`; its writer holds an exclusive advisory file lock for the
  full writer lifetime. At 16 MiB, a UTC date boundary, or clean shutdown, the owner calls
  `sync_data`, renames it to `.jsonl` while still holding the lock, and then releases the lock.
- Cleanup deletes only `.jsonl`. It may recover a crash-orphaned `.jsonl.active` only after acquiring
  that file's exclusive lock non-blockingly, diagnosing its tail, and renaming it to `.jsonl`.
  Liveness is never inferred from PID or modification time, and no extra lock file is introduced.
- The query layer merges the timeline by `occurred_at`, `observed_at`, component, segment, and
  sequence. It marks missing lifecycle events explicitly instead of inventing completion states.
- Legacy `audit.log` remains read-only. A compatibility reader projects version 0 `LogEntry` values
  as `policy.decision` events with stable synthetic event IDs; it does not rewrite, move, or delete
  legacy files in place.

### Preserve the five-crate architecture and existing owners

- `cosh-types` defines canonical Core and CLI types and remains side-effect free.
- `cosh-platform::audit` owns segment storage, compatibility reading, query, retention, and export.
- `cosh-shell` gains no internal crate dependency. It keeps a wire mirror in `types/audit.rs`,
  implements its producer in `journal/`, and proves bidirectional conformance with canonical golden
  fixtures.
- Do not add a root `crates/cosh-shell/src/*.rs` implementation file or serialize `InlineState`
  directly.

### Freeze SLS metrics completely

- Keep `TurnMetrics` fields, increment points, and per-turn reset semantics unchanged.
- Keep the exact 32 fields, names, types, calculations, and placeholders from `build_sls_record()`.
- Keep the default path, environment override, open/append/close behavior, and non-fatal failure
  semantics of `append_sls_log()`.
- Audit is an independent side path. It must not regenerate SLS from audit segments, add `trace_id`
  or any other SLS field, or change metric values or export timing when audit is enabled or disabled.
- A fixed session/config/metrics fixture must prove that audit enabled and disabled produce exactly
  the same SLS JSON.

## Alternatives Considered

### Extend SLS into the unified audit log

Rejected. SLS is aggregated turn telemetry and lacks complete event lifecycles and durable local
query. Adding fields would also break its production parsing contract.

### Keep one shared `audit.log` for every process

Rejected. `O_APPEND` does not prevent races when processes independently check size, rename files,
or clean them up. One faulty writer would also affect every producer's active file.

### Use SQLite or a resident audit daemon

Rejected for the first release. Both add schema migration, locking or service availability, and
deployment lifecycle concerns. Single-writer segments and a bounded reader satisfy the current
requirements. Remote real-time upload requires a separate ADR.

### Add a shared `cosh-audit` crate

Rejected for the first release. It would change the standalone `cosh-shell` dependency boundary
and the five-crate architecture. Canonical fixtures constrain the duplicated wire mirror; a leaf
crate can be evaluated separately if maintenance cost becomes unacceptable.

## Consequences

- The event protocol and disk layout become long-lived compatibility surfaces and require golden
  fixtures, unknown-event coverage, and version 0/version 1 compatibility tests.
- Each producer emits events at real semantic boundaries; UI rendering rows cannot become the fact
  source.
- Multi-segment querying requires stable cursors, a bounded reader, damaged or partial-line
  diagnostics, and deterministic cross-file ordering.
- The active suffix and lock make crash recovery explicit, but require lock and rename fault
  injection on every supported Unix platform.
- Independent SLS and audit paths add a small amount of I/O. Optimizations must not merge their
  schemas or alter SLS behavior.

## Follow-up

- Review and implement the linked Spec in stage order, starting with the version 1 contract,
  segment writer, legacy reader, and crash recovery.
- Keep the Core producer Spec's exact SLS compatibility gate as an implementation blocker.
- Keep the Shell producer Spec's wire-fixture conformance and layout audit as merge gates.
- Any future change to an SLS field, path, calculation, or failure behavior requires a separate
  reviewed work item and cannot be bundled into audit implementation.
