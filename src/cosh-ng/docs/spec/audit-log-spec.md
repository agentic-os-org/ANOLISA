# Unified Audit Log Implementation Specification

Date: 2026-07-22

Related documents: [Design](../design/audit-log.md),
[ADR-009](../adr/ADR-009-audit-event-segment-and-sls-contract.md), and
[ADR-010](../adr/ADR-010-audit-operations-retention-and-export-policy.md)

## Objective

Implement one stable, bounded, redacted audit timeline across `cosh-cli`, `cosh-core`, and
`cosh-shell` without changing SLS metrics, existing Shell evidence, or the standalone Shell crate
boundary. Delivery is split into five ordered stages inside this single Spec so each stage can be
implemented and verified independently while sharing one contract and acceptance gate.

## Delivery Model

| Stage | Outcome | Depends on |
| --- | --- | --- |
| 1 | Canonical event types, configuration, segment store, legacy reader, and retention planner | Accepted Design and ADRs |
| 2 | `cosh-cli audit` status, query, trace, retention dry-run, and redacted export | Stage 1 |
| 3 | Core Provider, Hook, Tool, approval, turn, and session producers with exact SLS compatibility | Stage 1 |
| 4 | Standalone Shell producers, audit references, and thin `/audit` UX | Stages 1-3 |
| 5 | Isolated integration and real Linux Shell/Core E2E validation | Stages 1-4 |

A stage may land only after its dependency gates pass. No stage may claim a complete production
timeline before both Core and Shell producers are connected and Stage 5 passes.

## Global Non-goals

- Raw prompts, Provider bodies, Tool arguments/results, commands, cwd, terminal output, environment
  variables, credentials, or full conversations.
- A user-facing audit disable setting.
- A new configuration file, evidence directory, workspace crate, daemon, database, remote upload
  protocol, signature, or hash chain.
- Replacing SLS, tracing, session persistence, Shell evidence, operating-system audit, or
  `cosh-shell diagnostics export`.
- Compliance-grade immutability against a malicious administrator.

## Global Prohibited Changes

- Do not modify `TurnMetrics`, its increment/reset sites, the 32 SLS fields, field types, values,
  order, calculations, placeholders, path, `COSH_SLS_LOG_PATH`, append timing, open flags, or
  non-fatal failure behavior.
- Do not generate SLS from audit events or audit events by parsing SLS.
- Do not rewrite, move, rotate, or delete legacy `audit.log` files.
- Do not fall back to `/tmp/cosh-audit.log` for version 1.
- Do not persist an unbounded JSON value or unknown field.
- Do not infer writer liveness from PID, file age, or modification time.
- Do not add configurable segment/record sizes, evidence retention, or `enabled`.
- Do not make `cosh-shell` depend on another internal workspace crate.
- Do not add a root `crates/cosh-shell/src/*.rs` implementation, a new `mod.rs`, an unregistered root
  re-export, or a `cosh_shell::...` self-crate path.

## Shared Configuration Contract

Use only the existing files:

1. `/etc/copilot-shell/config.toml`
2. `~/.copilot-shell/config.toml`
3. `<workspace>/.copilot-shell/config.toml`, detection only

```toml
[audit]
mode = "best_effort" # best_effort | required
retention_days = 30
max_disk_bytes = 1073741824
```

Rules:

- Built-in defaults are `best_effort`, 30 days, and 1 GiB.
- If the system file contains `[audit]`, its table is authoritative for all three settings. Omitted
  values use built-in defaults; user configuration cannot override them.
- If system `[audit]` is absent, user `[audit]` may override defaults.
- Ignore project `[audit]` as a whole and emit one stable warning naming the project file but not its
  values.
- Reject invalid mode, zero limits, overflow, and unknown `[audit]` keys. A system error cannot
  silently fall back to a weaker user setting.
- Existing `~/.copilot-shell/cosh/audit.toml` and `/etc/cosh/audit.toml` remain PDP-policy-only.
- `COSH_AUDIT_DIR` overrides only the storage root and must appear as the effective path source.

Platform owns the canonical loader. Shell mirrors only these audit rules and proves conformance
through shared language-neutral fixtures.

## Shared Storage Contract

Resolve the version 1 root in this order:

1. Non-empty `COSH_AUDIT_DIR`.
2. Non-empty `XDG_STATE_HOME` plus `cosh/audit`.
3. `HOME/.local/state/cosh/audit`.
4. No safe root, producing a typed unavailable result.

```text
audit/
  v1/
    segments/YYYY-MM-DD/
      <component>-<start_ms>-<pid>-<segment_id>.jsonl.active
      <component>-<start_ms>-<pid>-<segment_id>.jsonl
    state.json
    retention.lock
```

- Directories are `0700`; regular files are `0600`.
- Reject symlinks, non-regular files, unsafe existing permissions, owner mismatch, traversal, and
  overwrite races.
- File names contain only component enum, start milliseconds, PID, and UUID.
- Active writers hold a non-blocking exclusive advisory lock on their `.jsonl.active` file for the
  complete writer lifetime.
- On 16 MiB, UTC date change, or clean shutdown, the owner flushes, calls `sync_data`, renames to
  `.jsonl` while locked, then releases the lock.
- Cleanup directly handles only `.jsonl`. It may recover a crash-orphaned `.jsonl.active` only after
  acquiring that file's exclusive lock non-blockingly, diagnosing the tail, and renaming while
  locked. A lock failure always means skip.
- The lock is on the segment itself; no additional lock file or persistent path is introduced.
- One record is at most 64 KiB and consists of one UTF-8 JSON object plus newline.
- Security-boundary records call `sync_data` before success. Ordinary records flush after at most
  eight records or one second and on normal shutdown.

## Stage 1: Event Contract and Storage

### Scope

- Workspace dependencies.
- `crates/cosh-types/src/audit.rs` and canonical fixtures.
- `crates/cosh-platform/src/audit.rs` and children under `src/audit/`.
- Existing `crates/cosh-platform/src/audit/log.rs` as version 0 compatibility code.
- Configuration, writer, reader, state, crash recovery, and retention planning.

Migrate the existing `crates/cosh-platform/src/audit/mod.rs` to `src/audit.rs` before adding child
modules. Move the existing `uuid` version to `[workspace.dependencies]` and reuse it; do not add an
equivalent identifier crate.

### Canonical types

`cosh-types` must provide documented, side-effect-free types for:

- `AuditEventV1` with fixed `schema = cosh.audit.event` and `schema_version = 1`.
- Known event types plus a bounded unknown fallback.
- Component, identity, actor, outcome, subject, and redaction metadata.
- Typed producer payloads for every minimum event family in the Design.
- A bounded reader representation that preserves unknown event data without bypassing limits.
- `AuditMode`, effective settings, setting sources, query/status results, and stable audit errors.

Version 1 accepts new optional fields and unknown event types. Renames, unit changes, and narrower
values require version 2. Timestamps are UTC RFC 3339 milliseconds; durations are integer
milliseconds; sizes are bytes. Required identity minima are enforced by typed constructors.

### Fixtures

Add fixtures for:

- Every event family.
- Unknown event type and bounded unknown data.
- Legacy `LogEntry` plus expected `policy.decision` projection.
- Interior corruption and final partial line.
- Maximum-size and one-byte-oversize records.
- System/user/project configuration resolution.
- Platform/Shell wire conformance.

Tests compare exact field names, types, enum strings, and units, not only round-trip success.

### Writer and reader

- Create one UUID segment ID and sequence starting at zero per active segment.
- Use create-new and no-follow semantics and append only through the locked owner descriptor.
- Reject oversize records before writing any bytes.
- Writer errors expose safe operation/basename context but no event data.
- Discover active/closed version 1 segments and legacy files under bounded file, byte, record, and
  result limits.
- Honor `COSH_AUDIT_LOG` only as a legacy-reader override.
- Report invalid interior JSON, unsupported schema, read error, and active/final partial tail as
  distinct diagnostics. Only an incomplete final line may be omitted from results.
- Project legacy records with `legacy_schema = 0` and a stable synthetic ID derived from source
  identity, byte offset, and record hash. Never write the projection back.
- Order by `occurred_at`, `observed_at`, component, segment ID, and sequence.

### Operational state and retention

- Atomically replace private `state.json` containing schema, effective settings/sources, last
  successful write, and last write/retention/export error.
- Treat state as a last-observer operational cache, never an authorization source or audit ledger.
- Use `retention.lock` only for bounded cleanup coordination; timeout skips the pass.
- Recover eligible unlocked crash orphans before planning.
- Select closed segments beyond age first, then oldest closed segments until usage fits the byte
  cap. Never select locked active segments.
- Return deterministic candidate basename, bytes, timestamp, and reason. Planning and deletion are
  separate so dry-run and automatic cleanup share one plan.

### Stage 1 tests

- Exact fixture serialization, optional compatibility, bounded unknown events, and rustdoc.
- Defaults, system authority, user fallback, project rejection, invalid values, and source reporting.
- Modes, symlink/owner/path rejection, create-new behavior, and no safe root.
- Record limit, sequence, rotation, date rollover, flush class, rename/sync/lock failures.
- Two live writers never share a file or rename a segment locked by the other.
- Process exit releases the lock and enables deterministic orphan recovery.
- Legacy projection stability, corruption diagnostics, and fake-clock retention planning.

### Stage 1 gate

```bash
cargo fmt --all -- --check
cargo test --package cosh-types
cargo test --package cosh-platform
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```

## Stage 2: CLI, Retention, and Export

### Scope

- `crates/cosh-cli/src/cmd/audit.rs` and focused child modules as needed.
- Platform query, trace, retention execution, export, alias, and final-byte scanning.
- Audit result types and CLI integration tests.
- Required user documentation after implementation.

Keep `audit check` and `audit policy` unchanged. Add:

```text
cosh-cli audit status
cosh-cli audit events [filters] [--limit N] [--cursor CURSOR]
cosh-cli audit trace <id> [--since DURATION] [--until TIMESTAMP]
cosh-cli audit export [filters] --output PATH [--force]
cosh-cli audit prune --dry-run
```

Keep `audit log` for one compatibility cycle as a deprecated alias for
`events --event policy.decision`, reading legacy and version 1 data.

### Status, events, and trace

- Status reports root/source, mode, retention/source, state health, last errors, segment counts and
  bytes, event time bounds, reader diagnostics, and legacy discovery.
- Status may return a degraded payload when diagnostics are available; it fails only when evaluation
  cannot proceed safely. Paths are safe basenames/root labels only.
- Events supports time, event, component, outcome, identity, and schema-generation filters.
- Default page size is 100; maximum is 1,000.
- Cursor is opaque, versioned, bound to a normalized filter fingerprint, and contains only the last
  complete ordering key. Reject filter mismatch, malformed cursor, and unsupported cursor version.
- Page results include events, diagnostics, `next_cursor`, and safety truncation state.
- Trace matches an ID against `event_id` and all identity fields, reports matched identity kinds,
  orders one timeline, and computes durations only from compatible start/terminal pairs.
- Missing starts, duplicate starts, conflicting terminals, and missing terminals remain explicit
  gaps. Large traces continue through the same cursor model.

### Retention execution

- Schedule at most one bounded cleanup every 24 hours.
- Acquire `retention.lock` with bounded non-blocking retry.
- Recover unlocked crash orphans, execute the exact Stage 1 plan one file at a time, and preserve
  visible partial progress.
- Emit `retention.pruned` when possible and update operational state in all cases.
- `prune --dry-run` performs no rename or deletion and returns the exact plan.
- Version 1 has no manual destructive prune flag.

### Audit export

Require explicit `--output` and publish only:

```text
manifest.json
summary.json
events.jsonl
SHA256SUMS
```

- Select through the canonical bounded reader.
- Apply an export-specific typed allowlist and `audit-export-redaction-v1`.
- Alias installation/session/run/request/tool/command IDs consistently inside one bundle and
  differently across bundles.
- Manifest records tool/schema/policy versions, normalized filters, omissions, diagnostics, time
  range, and hashes. Summary contains only counts, failure classes, gaps, and aliases.
- Stage every private file, scan final bytes with the secret corpus and structural policy, then
  atomically publish the directory.
- Redactor/scanner/serialization/hash/permission/publish failure fails closed and removes staging.
- Refuse existing output. `--force` may replace only a directory with a valid cosh audit manifest,
  never a file, symlink, or unrelated directory.
- Emit `export.created` only after publication and never persist the full export path.
- Do not add `--include-raw` or evidence excerpts.
- Do not invoke, consume, modify, or regress `cosh-shell diagnostics export`.

### Stage 2 tests

- Clap parsing and `CoshResponse<T>` JSON/exit snapshots for all commands.
- Pagination limits, cursor continuation/mismatch/corruption, cross-segment order, unknown events,
  legacy alias, gaps, duplicates, and conflicts.
- Status under missing root, permissions, corrupt state, live writer, orphan, and legacy discovery.
- Dry-run plan equals automatic cleanup; locked active segments never appear.
- Alias scope, secret corpus, nested/oversize data, UTF-8 boundaries, and safe errors.
- Output symlink/file/unrelated directory/invalid manifest/scanner failure/staging cleanup.
- Existing Shell diagnostic export tests remain unchanged and passing.

### Stage 2 gate

```bash
cargo fmt --all -- --check
cargo test --package cosh-types
cargo test --package cosh-platform
cargo test --package cosh-cli --test cli_integration
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```

## Stage 3: Core Producers and SLS Compatibility

### Scope

Add a Core-owned recorder under `crates/cosh-core/src/` and connect only real semantic boundaries in
`core.rs`, `headless.rs`, `hook.rs`, `protocol.rs`, and narrow Tool/Provider adapters when ownership
is not available centrally.

The recorder provides typed methods, effective settings, identity propagation, a degraded latch,
security-boundary flush, ordinary emission, and injectable real/capture/no-op sinks. Production
always uses the real sink; no-op is test-only and is not configuration.

### Identity and events

- Preserve Provider session ID; generate one run ID per user-message execution and one turn ID per
  model turn.
- Preserve Provider request and Tool-use IDs when present. Never reuse session/run/turn/request/tool
  or command identities.
- One Provider permission, native stream, and foreground handoff for the same Tool use share
  `run_id + tool_use_id` and form one Tool lifecycle.
- Emit `session.started` after writer initialization and `session.ended` on normal shutdown.
- Emit `turn.started` before prompt Hooks/Provider and exactly one `turn.completed` or `turn.failed`.
- Durably emit `provider.request.started` before outbound request and exactly one completed/failed/
  cancelled terminal event.
- Emit one aggregated `hook.decision` per semantic Hook boundary, never per render notification.
- Emit `tool.requested`, approval request/resolution, execution start, and exactly one Tool terminal.
- Required IDs are constructor errors, not empty strings.

Persist only allowlisted metadata, hashes, categories, counts, durations, and opaque references.
Never persist Provider text/endpoints, Hook command/output/context, Tool input/result, approval
preview/free text, cwd, environment, or credentials.

### Failure modes

- `best_effort`: continue, expose one bounded warning/degraded state per episode, suppress warning
  storms, emit `audit.recovered` after the first successful durable write, then clear the latch.
- `required`: fail closed when Provider start, approval resolution, or Tool execution start cannot be
  durably flushed. Return a stable recoverable audit error before the action begins.
- A terminal write failure after a real side effect marks degradation and blocks the next governed
  transition until durable recovery; never claim the prior action did not happen.
- Audit failures do not increment Provider, Tool, Hook, approval, or SLS success/failure metrics.

### Frozen SLS gate

Run the production `build_sls_record()` path using identical fixed Core/session/config/metrics with:

1. A capture/no-op audit sink.
2. A real tempfile audit sink.

Assert identical serialized SLS bytes, the same 32 fields/types/values/order, the same append count
and turn boundaries, identical pre-created-file output, and unchanged missing/unwritable-file exit
behavior. The test-only no-op proves side-path independence; it is not a production disable switch.

### Stage 3 tests

- One terminal per started lifecycle across success, error, cancellation, Hook block, approval deny,
  and shutdown.
- Correct retry lineage without duplicate Tool executions.
- Required-mode barriers prove governed actions were not invoked.
- Post-side-effect terminal failure blocks only later governed work and reports the gap honestly.
- Best-effort degradation/recovery and secret-corpus exclusion from events/state/stderr/snapshots.
- Exact real/no-op SLS compatibility and all existing Core/Hook/Tool/Provider/protocol tests.

### Stage 3 gate

```bash
cargo fmt --all -- --check
cargo test --package cosh-core
cargo test --package cosh-core --test sls_integration
cargo test --package cosh-core --test tool_approval
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```

## Stage 4: Shell Producers and Troubleshooting UX

### Scope and conformance

- Add the version 1 wire mirror under `crates/cosh-shell/src/types/audit.rs`.
- Add audit config/writer/recorder code under existing `config/` and `journal/` owners.
- Use canonical JSON fixtures to prove Core/Platform events round-trip in Shell and Shell events
  validate canonically, including unknown optional data and exact units/enums.
- Mirror the shared config, private modes, root resolution, record limit, naming, lock, flush, rename,
  and crash behavior without copying query, retention, policy, or export code.
- Do not create another `mod.rs`; existing `journal/mod.rs` is pre-existing debt and remains behind
  the current facade unless a separately verified mechanical migration is required.

### Producer ownership

- Emit Shell command start and exactly one completed/failed event for native or approved foreground
  commands with stable shell-session and command IDs.
- Store only redacted first token, command/cwd hashes, duration, exit category, output bytes,
  truncation, and opaque `terminal-output://` reference.
- Native PTY commands continue on audit failure and show a persistent degraded gap.
- Core-owned approval requests carry optional `audit_ref`; Shell displays/journals the reference but
  does not re-emit the semantic request/resolution.
- Emit approval events only for Shell-owned governed actions with no Core producer. In required mode,
  resolution must be durable before execution.
- Emit `evidence.accessed` with scheme/type, bounded size/range category, and outcome only; never
  evidence content, excerpts, paths, or stale-reference detail.
- Provider Tool/activity rendering may carry `audit_ref` but never creates another audit fact.

### Projection and `/audit`

- Add optional event-ID `audit_ref` fields to approval, activity, cancellation, command details, and
  evidence result models. Never fabricate references from projection IDs.
- Implement `/audit status`, `/audit trace current`, and `/audit export current <dir>` by invoking
  bounded `cosh-cli audit` subprocesses and parsing `CoshResponse<T>`.
- Resolve `current` only from stable runtime IDs; require explicit export destination.
- Bound subprocess duration, output bytes, and parsed depth; all errors restore prompt/terminal state.
- Explain diagnostics export versus audit export in command help.

### Stage 4 tests

- Bidirectional fixture conformance and Platform-equivalent config fixtures.
- Distinct locked Shell segments and crash orphan behavior.
- Native command lifecycle contains no raw command/cwd/output.
- Core-owned approvals link without duplication; Shell-owned approvals produce one lifecycle.
- Required Shell-owned approval failure proves execution did not start.
- Evidence events contain only metadata/reference.
- Real references render in approval/activity/cancellation/details surfaces.
- `/audit` success, missing CLI, timeout, malformed/oversize JSON, query failure, and prompt recovery.
- Existing events, output refs, diagnostics, approval, activity, evidence, and layout tests remain green.

### Stage 4 gate

```bash
cargo fmt --all -- --check
cargo test --package cosh-shell --lib
cargo test --package cosh-shell --test logic
cargo test --package cosh-shell --test protocol
cargo test --package cosh-shell --test raw_cli <exact-audit-test> -- --exact
cargo test --package cosh-shell --test shell_host -- --test-threads=4
crates/cosh-shell/scripts/check-layout.sh
cargo clippy --workspace --all-targets -- -D warnings
```

Record actual exact test names; the placeholder above is not a claim that a command ran.

## Stage 5: End-to-End Validation

### Validation boundary

Unit/integration tests are diagnostic evidence, not final E2E. Final acceptance uses real
`cosh-shell`, its real `cosh-core` child, real local files/locks, and user-visible interaction driven
through `shell-use` on a Linux ECS or explicitly approved equivalent isolated Linux host.

Before ECS creation, cloud configuration, deployment, or E2E execution, present a test plan and
obtain user approval. This Spec does not itself authorize cloud spend or external mutation.

### Isolation

- Unique temporary root with isolated `HOME`, `XDG_STATE_HOME`, `COSH_AUDIT_DIR`, configs, and
  pre-created test-only `COSH_SLS_LOG_PATH`.
- Mock/test Provider with synthetic secrets only.
- Controlled process namespace, timeouts, process-group cleanup, and bounded artifacts.
- Recorded source commit, build command, binary hashes, OS/kernel/filesystem, and plan version.
- Never use the developer HOME, ambient config, production host, real credentials, or customer data.

### Local integration matrix

1. Concurrent Core/Shell/CLI writers have distinct locked active files and ordered query results.
2. A killed writer releases its lock; cleanup recovers only its orphan.
3. A live lock prevents rename/deletion during retention.
4. Interior corruption and final partial tail remain distinct diagnostics.
5. Fake-clock age/byte dry-run equals automatic cleanup.
6. Permission, no-root, symlink, owner, rename, sync, lock, and bounded disk-full failures.
7. Real/no-op audit produces byte-identical SLS.
8. Secret corpus is absent from segments, state, bundles, CLI/errors, stderr, and snapshots.

Faults unsafe on a real filesystem use narrow injected operations and remain labelled diagnostic,
not E2E.

### Real Shell scenarios

- Healthy workflow: real Provider fixture, safe Tool approval, native command, visible audit refs,
  `/audit` status/trace/export, and preserved prompt/terminal.
- Best-effort: reversible audit write failure, governed/native actions continue, one persistent
  degraded signal, permission recovery, `audit.recovered`, and cleared state.
- Required: separate failures before Provider, Shell-owned approval, and Tool execution prove each
  governed action did not start; native PTY remains usable and shows a gap.
- Crash/retention: dead writer orphan is recoverable while another live writer remains untouched.
- Export separation: diagnostics and audit exports preserve distinct defaults, formats, sources,
  and never read/rewrite one another.

### E2E plan and evidence gate

For each scenario, the user-reviewed plan includes purpose, prerequisites/cost, real Shell actions,
`shell-use` start/input/wait/assert/record steps, expected results, fail versus diagnostic criteria,
and complete rollback/cleanup.

If ECS is approved, run `aliyun configure list` before provisioning. Missing credentials stop for
user configuration; do not use OAuth or search for an alternative skill.

Record source/diff, commands/status/duration, scenario results, sanitized terminal artifacts,
manifests/hashes, SLS compatibility hashes, fault execution proof, and cleanup evidence. Separate
E2E conclusions from diagnostics. Any failed required scenario blocks shipment.

### Full repository gate

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
cargo doc --workspace --no-deps
crates/cosh-shell/scripts/check-layout.sh
```

## Completion Criteria

- Canonical version 1 fixtures are stable, bounded, and backward-readable.
- Concurrent live writers never share or mutate one another's active segments; crash recovery is
  lock-safe and deterministic.
- Operators can inspect status, page events, trace identities, preview retention, and export a
  fail-closed redacted bundle without live UI state.
- Core and Shell emit one correlated lifecycle per owned semantic action with no raw content.
- Required mode blocks only governed barriers; best-effort and native PTY remain usable and visible.
- SLS output, path, timing, and non-fatal behavior are exactly unchanged.
- Existing Shell evidence and diagnostics export remain compatible.
- Real Linux Shell/Core E2E passes and all resources are cleaned with evidence.

## Documentation Required with Implementation

New CLI commands and `[audit]` settings require updates to the component README summary and full
user-guide reference under the repository documentation standard. Design/ADR/Spec remain English;
user documentation follows its own required language structure. Do not update CHANGELOG outside a
release version-bump change.

## Risks

- Advisory locks may differ across filesystems; unsupported semantics fail visibly rather than
  proceeding unsafely.
- Mirrored Shell schema/writer logic can drift; canonical fixtures are a merge gate.
- Incorrect producer ownership can duplicate Tool/approval facts; every test identifies the owner.
- Terminal audit failure can occur after a real side effect; record the gap and block later governed
  work instead of pretending the prior action was rolled back.
- Final-byte scanning may produce false positives; export remains fail-closed and actionable.

## Open Questions

None. Another persistent path, schema-breaking field, raw evidence, shared crate, daemon/database,
remote sink, destructive manual prune, broader Shell reader/exporter, or SLS change returns to
Design and ADR review. ECS region, image, instance type, Provider fixture, cost, and timing are
execution-plan choices requiring user approval immediately before E2E.
