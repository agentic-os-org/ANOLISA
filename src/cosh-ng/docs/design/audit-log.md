# cosh-ng Unified Audit Log Design

Date: 2026-07-22

Related documents: [ADR-009](../adr/ADR-009-audit-event-segment-and-sls-contract.md),
[ADR-010](../adr/ADR-010-audit-operations-retention-and-export-policy.md), and
[Implementation Spec](../spec/audit-log-spec.md)

## Summary

cosh-ng should converge the existing policy-decision log, Core provider/tool lifecycles, and Shell
approval/command activity on a versioned `AuditEvent` contract. Each process appends only to its
own unique JSONL segment, and operational tools merge timelines by stable correlation IDs. SLS,
metrics, `events.jsonl`, and UI activity/details remain telemetry, PTY evidence, or projections
rather than audit sources of truth.

Version 1 persists only bounded structured metadata, outcome categories, summary hashes, and
opaque evidence references. It does not persist raw prompts, provider content, tool arguments or
results, terminal output, or environment variables. Audit events are retained for 30 days with a
1 GiB cap. Existing Shell evidence remains temporary and gains no new persistent directory.
Exports apply a stricter redaction pass and include a verifiable manifest.

Existing SLS metrics are a frozen compatibility contract. The path, 32 fields, field types and
calculations, turn export timing, `COSH_SLS_LOG_PATH`, and non-fatal failure behavior must not change.
Audit events are written independently; they add no SLS trace ID and are never used to regenerate
SLS records.

## Current State and Gaps

| Existing capability | Current fact | Missing production property |
| --- | --- | --- |
| `cosh-types::audit::LogEntry` | Records actor, `Action`, policy `Decision`, and source | No schema version, event ID, lifecycle, or cross-process correlation; policy decisions only |
| `cosh-platform::audit::log` | JSONL, `0600`, 16 MiB rotation, per-record `sync_data` | Shared-file rotation races; seven-file retention; silent cleanup and corrupt-line handling |
| `cosh-cli audit log` | Filters by session, outcome, time, and limit | Does not read rotations; no health, correlated trace, redacted export, or retention diagnostics |
| Core tool policy | Loads `LoadedPolicy`, but `classify_tool()` uses approval mode and tool kind | Core tool execution does not call `audit::check()`, so real agent decisions are not logged by the PDP |
| Core SLS/metrics | Aggregates tokens, API, tools, approvals, and latency | Turn counters are not a timeline; identity and diagnostic detail are missing; writes fail silently |
| Shell `events.jsonl` | Writes a redacted snapshot when the Shell host closes | Not an incremental durable journal and does not cover provider/tool/approval lifecycles |
| `cosh-shell diagnostics export` | Exports a bounded, redacted snapshot of environment, configuration, health, recent events, logs, and crashes | A general diagnostic bundle without stable audit events, correlation queries, retention, or audit-segment reader semantics |
| approval/activity/details | Shows requests, decisions, tool summaries, and cancellation artifacts | Mostly in-memory `InlineState` projections that cannot be queried after exit |

Several modules also reference a missing `docs/audit-design.md`. This document replaces that design
entry point, but it does not treat the current v0 `LogEntry` as a complete runtime audit contract.

## Goals

- Reconstruct a cross-Core/Shell production timeline from a session, run, request, tool, command,
  or event ID.
- Define a versioned, backward-readable, bounded stable file format.
- Apply explicit time and disk-budget retention, rotation, and cleanup rules.
- Produce incident bundles that are safe by default and demonstrably redacted.
- Provide a `cosh-cli audit` operational path independent of live UI state.
- Make audit failures visible and block unauditable governed execution in managed production mode.
- Preserve the standalone `cosh-shell` crate boundary and existing owner-module constraints.

## Non-goals

- Version 1 does not store or restore full conversations, terminal scrollback, raw tool inputs or
  outputs, or provider response bodies.
- Local files do not claim authenticity against a malicious administrator. Compliance-grade
  immutability belongs to an externally managed sink correlated by local trace IDs.
- SLS and tracing logs do not become audit sources of truth.
- The first phase adds no audit daemon, remote upload protocol, or new workspace crate.
- Audit retention does not replace session persistence, Shell evidence, or operating-system audit.
- Audit does not replace or change the general `cosh-shell diagnostics export` bundle contract.

## Concepts and Boundaries

### Keep four data classes separate

| Data | Purpose | Content boundary | Owner |
| --- | --- | --- | --- |
| Audit event | Who decided or executed what, when, why, and with what result | Bounded metadata, outcome, IDs, hashes, and references | Semantic producer; one writer per segment |
| Telemetry | Trends, capacity, SLOs, and alerts | Aggregated counts, latency, tokens, and error rates | `metrics.rs` / SLS exporter |
| Evidence | Investigation of a specific command or tool result | Terminal output, bounded excerpts, opaque references | `shell_host` / `evidence`, existing temporary lifecycle |
| Projection | Current activity, approval, and `/details` UI | A view derived from runtime state and audit events | Shell runtime/UI |

Audit events may reference evidence but never embed unbounded evidence. Telemetry retains the
existing `TurnMetrics` and SLS export path. Audit producers emit independently at the same runtime
boundaries and cannot mutate metrics or become an SLS data source.

### Ownership

- `cosh-types` defines canonical `AuditEventV1`, event payloads, filters, and export manifests used
  by Core and CLI while remaining side-effect free.
- `cosh-platform::audit` owns segment storage, compatibility reading, retention, query, and export.
  The current PDP emits `policy.decision` events through this layer.
- `cosh-cli` exposes stable machine-readable operational commands and does not duplicate parsing or
  redaction.
- `cosh-core` emits provider, hook, tool, approval, and turn events on an independent side path.
  Existing `TurnMetrics`, `build_sls_record()`, and `append_sls_log()` contracts remain unchanged.
- `cosh-shell` writes Shell, approval, and evidence events under `journal/`; `types/audit.rs` holds a
  wire mirror checked against canonical fixtures, avoiding a new internal-crate dependency.
  `ShellCommandAuditIdentity` remains a review-before-freeze support API because external Shell
  embedding and ledger consumers must name the transitive type of the public
  `ShellEvent.audit_identity` and `CommandBlock.audit_identity` fields for deserialization and
  struct construction; it is not a standalone stable API.
- `cosh-shell diagnostics/` continues to own the existing general diagnostic bundle. Version 1 does
  not make it read or interpret audit segments or change its default output and redaction behavior.
- `activity/`, `approval/`, and `runtime/details.rs` remain UI projection owners and expose stable
  `audit_ref` values instead of serializing `InlineState`.

The Shell wire mirror is an explicit trade-off to preserve the standalone boundary. Every v1 field
change must update canonical fixtures and Shell conformance tests. A shared leaf crate requires a
later ADR rather than an incidental dependency-direction change.

## File Layout and Single-writer Model

Default root resolution:

```text
$COSH_AUDIT_DIR
$XDG_STATE_HOME/cosh/audit/
$HOME/.local/state/cosh/audit/
```

There is no fixed `/tmp/cosh-audit.log` fallback when a safe state root is unavailable. The writer
enters a degraded state and applies the configured failure mode.

```text
audit/
  v1/
    segments/
      2026-07-22/
        cosh-cli-<start_ms>-<pid>-<segment_id>.jsonl.active
        cosh-core-<start_ms>-<pid>-<segment_id>.jsonl
        cosh-shell-<start_ms>-<pid>-<segment_id>.jsonl
    state.json
    retention.lock
  legacy/
    README
```

Each process creates a random `segment_id` and appends only to files it created. An active writer
uses the `.jsonl.active` suffix and holds an exclusive advisory lock on the file descriptor for its
entire lifetime. At 16 MiB, a UTC date boundary, or clean shutdown, the owner calls `sync_data`,
renames the file to `.jsonl` while still holding the lock, and only then releases the lock. No
process renames a segment whose lock is held by another process.

Cleanup never infers liveness from PID or modification time. It deletes only `.jsonl` files. A
crash-orphaned `.jsonl.active` file becomes eligible for recovery only after cleanup acquires its
exclusive lock non-blockingly; cleanup then diagnoses any partial tail, renames it to `.jsonl`, and
applies normal retention. The lock is held on the segment itself, so this protocol adds no lock file
or persistent path.

Security and durability requirements:

- Directories use `0700`; files use `0600`; opens reject symlinks, non-regular files, and ownership
  mismatches.
- Active-segment locks are advisory process-lifetime locks. Lock acquisition failure is never
  treated as proof that a segment is closed.
- File names contain only internal enums, integers, and random IDs, never user, session, workspace,
  or tool input.
- A record is at most 64 KiB. Oversized fields are policy-truncated or dropped, never emitted as
  unbounded JSON.
- Security-boundary events call `sync_data` before execution resumes. Ordinary lifecycle events may
  batch at most eight records or one second and flush during normal shutdown.
- Readers may ignore only an incomplete final line and report `trailing_partial_record`. Invalid
  interior lines, unknown schemas, and read errors are diagnostics rather than silent skips.

## Stable Event Format

Each line is one complete UTF-8 JSON object:

```json
{
  "schema": "cosh.audit.event",
  "schema_version": 1,
  "event_id": "018f...",
  "event_type": "tool.completed",
  "occurred_at": "2026-07-22T08:12:01.123Z",
  "observed_at": "2026-07-22T08:12:01.125Z",
  "sequence": 42,
  "component": {"name": "cosh-core", "version": "0.12.0"},
  "identity": {
    "installation_id": "inst_...",
    "shell_session_id": "...",
    "provider_session_id": "...",
    "run_id": "...",
    "turn_id": "...",
    "request_id": "...",
    "tool_use_id": "...",
    "command_id": null
  },
  "actor": {"kind": "user", "uid": 1000, "euid": 1000},
  "outcome": {"status": "success", "code": null, "retryable": false},
  "subject": {"kind": "tool", "name": "shell"},
  "data": {"duration_ms": 52, "output_bytes": 144, "output_ref": "terminal-output://..."},
  "redaction": {"policy_version": "audit-redaction-v1", "status": "clean", "fields": []}
}
```

### Compatibility rules

- `schema` and `schema_version` version the file contract, not the product.
- Version 1 permits only new optional fields, event types, and unknown enum fallbacks. Renames, unit
  changes, and narrower accepted values require a v2 segment directory.
- Writers validate their own events strictly. Readers preserve unknown event types as bounded data
  while still querying the public envelope.
- Timestamps are UTC RFC 3339 milliseconds; durations are integer milliseconds; sizes are bytes.
- `event_id` is globally unique and `sequence` increases only within a segment. Cross-segment order
  uses `occurred_at, observed_at, component.name, segment_id, sequence` and does not assume a perfect
  host clock.
- Identity fields are optional globally, with per-event minima. Tool events need `run_id` and
  `tool_use_id`; approval events need `run_id` and `request_id`; Shell commands need
  `shell_session_id` and `command_id`.

### Minimum v1 event catalog

| Domain | Events |
| --- | --- |
| Session/turn | `session.started`, `session.ended`, `turn.started`, `turn.completed`, `turn.failed` |
| Provider | `provider.request.started`, `provider.request.completed`, `provider.request.failed`, `provider.request.cancelled` |
| Policy/Hook | `policy.decision`, `hook.decision` |
| Tool | `tool.requested`, `tool.execution.started`, `tool.completed`, `tool.failed`, `tool.cancelled` |
| Approval | `approval.requested`, `approval.resolved` |
| Shell/evidence | `shell.command.started`, `shell.command.completed`, `shell.command.failed`, `evidence.accessed` |
| Audit control | `audit.degraded`, `audit.recovered`, `retention.pruned`, `export.created` |

A provider stream, control permission, and foreground handoff for one tool share
`run_id + tool_use_id`; they are lifecycle events, not three executions. A Shell handoff adds
`command_id`. Readers display missing starts or terminals as gaps instead of inventing completion.

An approval whose request could not be sent is still terminal: Core writes `approval.resolved` with
`outcome.status=failed`, `decision=emit_failed`, and `reason_code=control_transport_<class>` when the
control request could not be serialized, written, or flushed. The claim is that this producer never
obtained a decision, not that the request was provably lost — a failed write may still have put bytes
on the wire, so delivery could not be confirmed either way. Readers must not fold it into a user
`deny`: nobody decided anything.

## Field Minimization and Redaction

### Default persisted content

- Provider: provider type/ID, model, latency, token counts, finish/error category, and retry count;
  never endpoint query parameters, credentials, prompts, or responses.
- Tool: canonical name, kind, input shape and hash, execution path, duration, result category,
  stdout/stderr line and byte counts, truncation, and opaque output reference; never raw arguments or
  results.
- Approval: risk and assessment, actor kind, decision, reason code, wait time, preview hash, and
  redaction state; never raw preview or free-form reason.
- Shell: a redacted first program token, command hash, cwd scope hash, exit code, duration, output
  bytes/reference; never full command, cwd, or output.
- Hook: canonical hook ID, decision, duration, and reason code; never arbitrary stderr or context.

### Redaction pipeline

1. Producers construct allowlisted typed payloads; unknown fields cannot enter an event.
2. Paths, targets, workspaces, and user labels use an installation-scoped salted hash. Random
   session/run UUIDs remain for local correlation.
3. Permitted short text passes through a general secret scanner and UTF-8 byte bounds.
4. `redaction.status` is `clean`, `redacted`, `dropped`, or `failed_closed`; field paths are listed
   without recording secret types or values.
5. Export applies stricter `audit-export-redaction-v1` processing and scans final bundle bytes. An
   internal redactor or scanner failure fails the export closed.

The legacy `redacted: bool` survives only as v0 compatibility metadata.

## Write-failure Semantics

No new configuration file is introduced. Runtime audit settings are accepted only from the existing
system and user configuration:

- system: `/etc/copilot-shell/config.toml`
- user: `~/.copilot-shell/config.toml`
- project: `[audit]` in `<workspace>/.copilot-shell/config.toml` is ignored with a warning

Only three optional settings are added; built-in defaults apply when they are absent:

```toml
[audit]
mode = "best_effort" # best_effort | required
retention_days = 30
max_disk_bytes = 1073741824
```

Audit is always enabled and has no ordinary disable setting. The 16 MiB segment size is an
implementation constant. System configuration is authoritative when present, and user settings
cannot downgrade `required` to `best_effort`. Existing `~/.copilot-shell/cosh/audit.toml` and
`/etc/cosh/audit.toml` remain PDP-policy files only. `COSH_AUDIT_DIR` is an explicit operations/test
path override.

- `best_effort` is the local default. Work continues, but stderr, Shell health, and `audit status`
  continuously report degradation; recovery emits `audit.recovered`. Failure is never silent.
- A managed system policy may require `required`. Provider requests, agent tools, approval
  resolutions, and governed mutations fail closed when their critical record cannot be durably
  flushed.
- Native commands typed directly into the PTY are not blocked solely by cosh-ng audit failure. They
  create a visible gap. Environments requiring all keystrokes must also deploy system audit.
- A telemetry/SLS sink failure does not affect local audit, and an audit failure cannot be hidden by
  successful telemetry.

## Retention and Cleanup

Audit defaults to 30 days with a 1 GiB cap. Managed configuration may override these values, and
`audit status` reports the effective values and their source. Version 1 adds no persistent evidence
directory. Existing Shell `events.jsonl` and `output-refs` lifecycles remain unchanged; audit stores
only opaque references, hashes, types, and sizes.

1. Cleanup handles closed `.jsonl` segments and never deletes a locked `.jsonl.active` segment.
   An orphaned active segment is recovered only after a successful non-blocking exclusive lock.
2. It runs asynchronously after writer startup and at most every 24 hours; CLI supports
   `prune --dry-run`.
3. It deletes segments older than `retention_days`, then continues oldest-first if the byte cap is
   exceeded.
4. Cleanup uses a bounded lock and bounded work. Lock timeout skips the pass without blocking an
   execution path.
5. Results, bytes, reasons, and errors go to `retention.pruned` or `state.json`.
6. Explicit export bundles are user-owned and outside automatic retention. `--output` is required.

## Redacted Export

Current `main` already provides `cosh-shell diagnostics export`. It remains a general best-effort
snapshot of environment, configuration, health, recent events, logs, and crashes. The following
`cosh-cli audit export` is a separate audit-specific bundle that consumes only the version 0/version
1 audit store and supplies stable schemas, correlation IDs, gaps, and an integrity manifest. The
first release does not make either command invoke or read the other, preserving Shell's standalone
boundary and existing diagnostic behavior.

```text
cosh-cli audit export \
  --session <shell-or-provider-session-id> \
  --since 2h \
  --output ./cosh-audit-incident/
```

The v1 bundle is a `0700` directory:

```text
cosh-audit-incident/
  manifest.json
  summary.json
  events.jsonl
  SHA256SUMS
```

- `manifest.json` records tool version, filters, source and export schemas, redaction policy,
  omitted counts, corrupt/partial records, time range, and file hashes.
- `summary.json` contains counts, failure classes, important gaps, and identity aliases only.
- `events.jsonl` contains re-redacted v1 events. Installation/session/run identities become aliases
  stable only within that bundle.
- Version 1 intentionally has no `--include-raw`. Evidence exports only type, size, hash, and opaque
  reference. Bounded excerpts require a later ADR and explicit consent.
- Existing directories are not overwritten. `--force` may replace only a directory with a valid
  cosh audit manifest.

## Production Troubleshooting Surface

Keep `cosh-shell diagnostics export` as the broad incident snapshot. Use the following
`cosh-cli audit` commands when troubleshooting requires stable event correlation, audit-retention
health, or an exact timeline.

Keep existing `check` and `policy`, and add:

```text
cosh-cli audit status
cosh-cli audit events --since 2h --event tool.failed,provider.request.failed
cosh-cli audit trace <event|session|run|request|tool|command-id>
cosh-cli audit export --session <id> --since 2h --output <dir>
cosh-cli audit prune --dry-run
```

- `status` reports paths, mode, retention, disk use, time bounds, segments, corrupt/partial counts,
  and last write/retention/export errors.
- `events` uses bounded pages and stable cursors and never eagerly loads all segments.
- `trace` returns a correlated timeline, durations, and gaps in `CoshResponse<T>`.
- Existing `audit log` remains a one-cycle alias for `events --event policy.decision` and reads both
  legacy `audit.log` and v1 segments.
- Shell later provides `/audit status`, `/audit trace current`, and `/audit export current <dir>` as
  a thin current-session UX over the canonical store semantics.
- Activity, approval, and `/details` cards expose `audit_ref: <event_id>` for direct tracing.

## Legacy Compatibility and Migration

- Legacy `audit.log` and rotations remain read-only and are never rewritten or moved automatically.
- A compatibility reader projects each `LogEntry` as `policy.decision`. It derives a stable synthetic
  event ID from source-file identity, offset, and record hash and marks `legacy_schema = 0`.
- New releases write only v1 segments while log/events/trace/export read v0 and v1.
- Stopping v0 discovery requires at least one compatibility cycle; deleting old files is explicit.
- Shell `events.jsonl` remains PTY evidence and is not imported as AuditEvent, because historical
  free text has not met the v1 redaction contract.
- Existing diagnostic bundles are explicit exports, not audit-reader inputs or legacy audit sources.

## Delivery Phases

1. [Event and storage](../spec/audit-log-spec.md#stage-1-event-contract-and-storage): add canonical fixtures, v0 compatibility,
   configuration resolution, locked active segments, crash recovery, and the v1 store.
2. [CLI, retention, and export](../spec/audit-log-spec.md#stage-2-cli-retention-and-export): deliver
   status/events/trace/export/prune so schema and failure behavior are independently testable.
3. [Core producers](../spec/audit-log-spec.md#stage-3-core-producers-and-sls-compatibility): integrate Provider, Hook, Tool, approval,
   and turn side-path events; prove that SLS JSON, path, call timing, and failure semantics remain
   unchanged.
4. [Shell producers](../spec/audit-log-spec.md#stage-4-shell-producers-and-troubleshooting-ux): integrate Shell-owned approval, command,
   and evidence lifecycles under `journal/`, expose `audit_ref`, and add the thin `/audit` surface.
5. [E2E validation](../spec/audit-log-spec.md#stage-5-end-to-end-validation): test real Shell/Core multi-process
   operation, disk/permission faults, crash recovery, retention, and secret export scans in an
   isolated Linux environment.

No phase may claim a complete production timeline before the corresponding producers are connected.

## Acceptance Criteria

- Core, Shell, and CLI processes never share an active segment or rename a segment locked by
  another process.
- A crashed writer leaves `.jsonl.active`; cleanup cannot reclaim it while its lock is held and can
  recover it deterministically after the process exits.
- Any stable ID yields an ordered `audit trace` with explicit gaps.
- v0 and v1 fixtures query, filter, and export together; unknown v1 events do not poison a file.
- Interior corruption and crash-tail partial records are reported rather than silently disappearing.
- The 30-day/1 GiB retention policy executes deterministically without changing existing Shell
  evidence lifecycles.
- Secret corpora and randomized nested inputs do not appear in segments, bundles, manifests, CLI
  errors, or test snapshots.
- `required` blocks Provider/Agent governed execution on critical flush failure; `best_effort`
  persistently reports degradation and records recovery.
- With audit enabled or disabled, identical fixed inputs produce identical SLS JSON; the SLS path,
  32 fields, calculations, export timing, and non-fatal failure behavior remain unchanged.
- Shell adds no internal-crate dependency or root implementation file and introduces no layout
  violation group.
- Existing `cosh-shell diagnostics export` CLI, default output, and redaction regressions remain
  unchanged; audit export does not scan or rewrite its bundles.

## Accepted Decisions and Remaining Risks

The maintainer confirmed on 2026-07-22:

1. SLS metrics are a frozen compatibility contract. Audit cannot change their fields, values, path,
   timing, or failure behavior.
2. No configuration file is added. Optional `[audit]` settings use existing system/user files, and
   project configuration has no authority over them.
3. Audit gains an independent segment path, but v1 adds no persistent evidence path or raw evidence.
4. Local mode defaults to `best_effort`; system configuration may require `required`, which fails
   closed only for Provider/Agent governed execution, not native PTY commands.
5. Version 1 exports prohibit raw prompts, tool arguments/results, and terminal evidence and retain
   only hashes and references.
6. Shell keeps a wire mirror plus canonical fixtures and adds no internal-crate dependency.
7. Version 1 provides durability and traceability without a keyless hash chain or local signing;
   managed external sinks provide compliance immutability.
8. Active segments use `.jsonl.active` plus a process-lifetime file lock; the owner closes by
   renaming to `.jsonl`, and cleanup recovers only unlocked crash orphans.

ADR-009 and ADR-010 lock these choices. Remaining risks are multi-process failures, full disks,
redactor misses, legacy compatibility, and operator choice between two export surfaces. Staged specs,
command help, and isolated E2E must cover them with fault injection and secret corpora.
