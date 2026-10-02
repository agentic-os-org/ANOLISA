# Security Audit

[中文版](../../../../zh/user-entrypoint/cosh-ng/cli/audit.md)

`cosh-cli audit` checks whether an action is allowed and reads the redacted audit events used for troubleshooting. It supports policy checks, bounded queries, correlated traces, incident exports, and retention previews. Every command returns the standard `CoshResponse<T>` JSON envelope.

## Commands

| Command | Purpose |
|---|---|
| `cosh-cli audit check` | Evaluate an action under the active policy |
| `cosh-cli audit log` | Read policy-decision events for a session |
| `cosh-cli audit status` | Show audit storage and reader health |
| `cosh-cli audit events` | Query a bounded page of events |
| `cosh-cli audit trace <id>` | Follow events for an ID or correlation identity |
| `cosh-cli audit export --output <dir>` | Write a redacted incident bundle |
| `cosh-cli audit prune --dry-run` | Preview retention candidates |
| `cosh-cli audit policy ...` | Inspect or validate policy files |

Use `cosh-cli audit --help` or an action's `--help` output for the complete option list.

## Check a policy decision

Pass either a raw action string or structured fields:

```bash
cosh-cli audit check --action-string "pkg install nginx"
cosh-cli audit check --subsystem pkg --operation install --target nginx
cosh-cli audit log --session abc123 --since 2h --limit 50
```

`--action` remains an alias for `--action-string`. Structured checks require `--subsystem` and `--operation`; `--target` and paired `--arg-key`/`--arg-value` fields are optional.

## Query and export events

```bash
cosh-cli audit status
cosh-cli audit events --since 2h --event approval.requested,approval.resolved --limit 100
cosh-cli audit trace 7fa4c0b0-0000-4000-8000-000000000001
cosh-cli audit export --since 2h --identity session-123 --output ./audit-incident
cosh-cli audit prune --dry-run
```

`--since` accepts a duration such as `30s`, `5m`, `2h`, or `1d`, or an RFC 3339 timestamp. `--until` accepts an RFC 3339 timestamp. `events` and `export` also accept repeated or comma-separated `--event`, `--component`, and `--outcome` filters, plus `--identity` and `--schema v1|legacy_v0`; `events` and `trace` support an opaque `--cursor` for the next page.

Inside `cosh-shell`, `/audit status`, `/audit trace current`, and `/audit export current <dir>` provide bounded wrappers for the same operations.

An export contains `events.jsonl`, `summary.json`, `manifest.json`, and `SHA256SUMS`. The export is redacted and published atomically; `--force` replaces only a directory containing a valid cosh audit manifest. Version 1 supports retention preview only, so `audit prune` must include `--dry-run` and does not delete data.

## Policy commands

```bash
cosh-cli audit policy show
cosh-cli audit policy list
cosh-cli audit policy validate ./audit.toml
cosh-cli audit policy explain "cat /etc/os-release"
```

The policy loader also accepts the legacy `cosh-cli audit check --action ...` form. For policy locations, audit settings, and storage overrides, see [Configuration](../configuration.md). System audit settings take precedence over user settings; project audit tables are ignored.
