# ADR-010: Audit Configuration, Storage, Failure, Retention, and Export Policy

Date: 2026-07-22

Related documents: [Design](../design/audit-log.md) and
[Implementation Spec](../spec/audit-log-spec.md)

## Context

Unified audit events need an explicit storage location, disk limit, write-failure semantics, and a
safe export boundary. Current `main` already has `cosh-shell diagnostics export`, which produces a
general sanitized snapshot but is not a stable audit-event contract. A separate configuration file,
workspace authority to disable audit, or raw prompt, tool, and terminal content in exports would
expand configuration fragmentation, supply-chain input, and secret-disclosure risks. Fully
hard-coded retention and failure modes would not support both local development and managed
production environments.

## Decision

### Do not add a configuration file

Audit is always enabled and has no ordinary disable switch. Add only an optional `[audit]` table to
the existing configuration files:

```toml
[audit]
mode = "best_effort" # best_effort | required
retention_days = 30
max_disk_bytes = 1073741824
```

- System: `/etc/copilot-shell/config.toml`.
- User: `~/.copilot-shell/config.toml`.
- Project: ignore `[audit]` in `<workspace>/.copilot-shell/config.toml` and emit a warning.
- System configuration is authoritative when present. User configuration cannot downgrade
  `required` to `best_effort`.
- The 16 MiB segment size is an implementation constant, not a user setting.
- `~/.copilot-shell/cosh/audit.toml` and `/etc/cosh/audit.toml` remain PDP-policy-only files and do
  not acquire storage or retention settings.

### Add an independent audit path but no persistent evidence path

Resolve the path in this order:

```text
$COSH_AUDIT_DIR
$XDG_STATE_HOME/cosh/audit/
$HOME/.local/state/cosh/audit/
```

When no safe state root exists, do not fall back to a fixed `/tmp/cosh-audit.log`; enter the
degraded or required failure path. `COSH_AUDIT_DIR` is an explicit operations and test override,
and `audit status` must report its effective source.

```text
audit/
  v1/
    segments/YYYY-MM-DD/*.jsonl
    state.json
    retention.lock
```

- Create directories as `0700` and files as `0600`; reject symlinks, non-regular files, and owner
  mismatches.
- Atomically update `state.json` with the last successful write, last write/retention/export error,
  and effective configuration source.
- Use `retention.lock` only for bounded cleanup coordination, not business state.
- Keep existing temporary Shell `events.jsonl`, `output-refs`, and work-directory lifecycles
  unchanged. Version 1 stores only opaque references, hashes, types, and sizes and creates no new
  evidence directory.
- Read legacy `$XDG_STATE_HOME/cosh/audit.log` or `$HOME/.local/state/cosh/audit.log` only for
  compatibility.

### Define failure modes

- `best_effort` is the local default. Work continues, but stderr, Shell health, and `audit status`
  continuously show degraded state. Recovery writes `audit.recovered`; failures are never silent.
- System configuration may require `required`. Durable flush failure for critical Provider
  requests, agent tools, approval resolution, or governed mutation fails closed.
- Direct user commands in the native PTY are not blocked by a cosh-ng audit failure. The Shell must
  display the audit gap. Environments that must capture all user input also deploy operating-system
  audit facilities.
- SLS and telemetry success or failure is independent of the audit failure mode.

### Apply bounded retention

- Default to 30 days and 1 GiB, whichever limit is reached first.
- Clean only closed `.jsonl` segments. Recover `.jsonl.active` crash orphans only after obtaining
  their exclusive file lock non-blockingly; never delete a locked active segment.
- Delete by age first, then oldest-first while usage exceeds the byte cap.
- Run cleanup asynchronously after writer startup and at most once every 24 hours. Skip on lock
  timeout without blocking execution.
- `cosh-cli audit prune --dry-run` returns exact candidates, bytes, and reasons.
- Record cleanup results in `retention.pruned`; if that event cannot be written, still record the
  last error in `state.json`.

### Export only redacted incident bundles

- Keep the existing `cosh-shell diagnostics export` command unchanged as a general best-effort
  diagnostic snapshot. It does not become an audit reader or audit retention surface.
- `cosh-cli audit export` is a distinct audit-only contract. It reads only version 0/version 1 audit
  storage and does not invoke, consume, or rewrite Shell diagnostic bundles.
- `cosh-cli audit export` requires explicit `--output` and creates a `0700` bundle directory by
  default.
- A bundle contains only `manifest.json`, `summary.json`, re-redacted `events.jsonl`, and
  `SHA256SUMS`.
- Version 1 has no `--include-raw`. It does not export raw prompts, Provider bodies, tool
  arguments/results, full commands, cwd, terminal output, environment variables, or secrets. It
  retains only bounded metadata, hashes, and opaque references.
- The exporter applies a separate policy stricter than at-rest redaction and scans final bytes. A
  redactor or scanner failure fails closed and leaves no incomplete bundle.
- Installation, session, and run IDs use bundle-stable aliases to prevent cross-incident
  correlation.
- Export bundles are explicit user-owned artifacts outside audit retention and do not overwrite an
  existing directory by default.

## Alternatives Considered

### Add a separate `audit-config.toml`

Rejected. It would add a fourth discovery and precedence system and would be easy to confuse with
existing Core and Shell configuration or PDP `audit.toml`.

### Let project configuration control audit

Rejected. An untrusted workspace could disable audit, rewrite its path, or shorten retention and
break production traceability.

### Continue using one legacy `audit.log`

Rejected. It cannot safely represent versioned multi-producer segments, health diagnostics, and
age and byte cleanup.

### Export raw evidence for easier troubleshooting

Rejected. Raw prompts, tool results, and terminal output have the highest disclosure risk. A future
bounded excerpt requires a new ADR, explicit consent, a separate sensitivity policy, and end-to-end
secret scanning.

### Reuse the Shell diagnostic bundle as the audit export contract

Rejected. Its sources, single-file format, default output behavior, and best-effort purpose differ
from the stable audit schema and retention model. Reusing it would also duplicate canonical audit
reading in the standalone Shell crate or change that crate's dependency boundary.

### Fail closed in every environment

Rejected. Local filesystem failures would break Shell-first availability and incorrectly block
native PTY commands. Managed `required` covers only cosh-governed Provider and Agent behavior.

## Consequences

- Core, Shell, and CLI must share `[audit]` defaults and effective-source semantics. Project-level
  forbidden fields need stable warnings and tests.
- The directory may consume up to 1 GiB of user state and needs clear `status`, `prune --dry-run`,
  and cleanup diagnostics.
- Incident bundles are safe by default but contain no raw content; troubleshooting relies on event
  correlation, outcome classification, hashes, and evidence references.
- Operators retain a broad Shell diagnostic export and gain a separate audit-only export. Command
  help must make the selection rule explicit.
- `state.json` is operational health state, not an audit fact ledger, and cannot replace segment
  events.

## Follow-up

- Review and implement the linked Spec stages for precedence, project
  rejection, path hardening, bounded pagination, retention, aliases, manifests, and final-byte
  secret scanning.
- Isolated end-to-end tests must cover missing HOME/XDG, symlinks, permission denied, disk full,
  crash tails, cleanup locks, export overwrite, and a secret corpus.
- A future raw or bounded evidence export, remote sink, or signature chain requires a new ADR.
