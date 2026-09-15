# AgentSecCore Internal Commands

[中文版](../../../zh/agent-security/agent-sec-core/internal-commands.md)

`--trace-context` and `log-sandbox` are hidden integration points used by hooks
and plugins. Their contracts are documented here for audit and debugging.
`hidden=True` controls help visibility only; it does not establish availability.

## Inventory

| Surface | Role | Invoking it |
| --- | --- | --- |
| `--trace-context JSON` (top-level option) | Hidden integration point | Works |
| `log-sandbox` | Hidden integration point | Works |
| `skill-ledger rotate-keys` | Visible, not implemented | Explicit error, exit `1` |

The two hidden integration points support explicit `--help`.

## `log-sandbox`

Records one sandbox pre-hook decision as a Security Event. This is the audit
half of Copilot Shell's sandbox protection: the
[`sandbox-guard` hook](../../user-entrypoint/copilot-shell/hooks.md) decides how
to treat a dangerous shell command, then spawns `log-sandbox` so the decision
lands in the local event store.

```bash
agent-sec-cli log-sandbox \
  --decision sandbox \
  --command 'rm -rf /tmp/test' \
  --reasons 'recursive-delete' \
  --network-policy restricted \
  --cwd /home/user/project
```

### Options

Every option is a free-form string and defaults to empty. Nothing is rejected or
normalized — the value you pass is what gets recorded.

| Option | Recorded meaning |
|--------|------------------|
| `--decision` | The pre-hook verdict. `sandbox-guard` only ever emits `block` or `sandbox` |
| `--command` | The shell command that was evaluated |
| `--reasons` | Why the decision was made, as produced by the caller's rule labels |
| `--network-policy` | Network posture of the sandboxed run: `restricted` or `enabled`. Omitted on the `block` path, so it lands as an empty string |
| `--cwd` | Working directory the command was about to run in |

Those are the values `sandbox-guard` produces, not values the CLI enforces.
Nothing is validated against an allowed set, so an unexpected value is stored
verbatim and a typo becomes a silently mislabeled audit record rather than an
error. If you filter events on these fields, filter on what the hook actually
emits — there is no `allow` record and no `unrestricted` policy.

### Output and exit codes

The command is silent by design — no stdout, no stderr on success, because the
caller spawns it detached and never reads its output.

| Exit code | Condition |
|-----------|-----------|
| `0` | The logging-only backend ran to completion. This is the normal outcome, and it is *not* a confirmation that the event reached storage |
| Non-zero | An unexpected internal failure propagated out of the middleware |

Event writing itself is best-effort: if the JSONL or the SQLite writer fails, the
failure is swallowed and the exit code stays `0`. Never treat exit `0` as proof
that a record exists — query the event store instead, as shown in
[Verifying the record](#verifying-the-record). Combined with the fact that
`sandbox-guard` spawns the process detached with output discarded, the exit code
has no influence on sandbox enforcement — losing an audit record never blocks or
unblocks a command.

### What it does not do

`log-sandbox` and `linux-sandbox` are easy to confuse. Only one of them isolates
anything.

| | `linux-sandbox` | `agent-sec-cli log-sandbox` |
|---|---|---|
| Form | Standalone binary at `/usr/local/bin/linux-sandbox` | Hidden `agent-sec-cli` subcommand |
| Role | Actually runs a command under filesystem and network isolation | Records that a decision was made |
| Effect on the command | Wraps and executes it | None — it never executes, blocks, or rewrites anything |
| How `sandbox-guard` uses it | Rewrites the tool call to run through it | Spawns it detached, fire-and-forget |

So a `--decision block` record does not block anything. The blocking already
happened in the hook; `log-sandbox` only makes it auditable.

### Verifying the record

Sandbox decisions are stored with event type `sandbox_prehook` under category
`sandbox`:

```bash
agent-sec-cli events --category sandbox --last-hours 1
agent-sec-cli events --event-type sandbox_prehook --output json --limit 5
```

In the JSON output, `details.request` holds the five options as passed, and the
decision is at `details.result.decision`. Events live in
`/var/log/agent-sec/` when writable, otherwise `~/.agent-sec-core/`;
`AGENT_SEC_DATA_DIR` overrides both.

## `--trace-context`

A top-level option that lets a calling plugin attach its own correlation IDs to
every Security Event the invocation produces, so security records can be joined
with the host Agent's traces.

```bash
agent-sec-cli --trace-context '{"trace_id":"t-1","session_id":"s-1"}' \
  log-sandbox --decision block --command 'rm -rf /'
```

The value is a JSON object. Recognized fields are `trace_id`, `session_id`,
`run_id`, `call_id`, `tool_call_id`, and `agent_name`; each also accepts its
camelCase spelling (`traceId`, `sessionId`, …). Unknown fields are ignored, and
values longer than 256 characters are truncated with a `...[truncated]` marker.

The first five land on the Security Event as correlation columns. `agent_name` is
the exception: it is carried as Observability metadata (`component.agent_name`)
and is not stored on the event, so do not expect to filter events by it.

It must appear before the subcommand — it is a process-level option, parsed
ahead of the command so that a subcommand's own flags keep their meaning.
Malformed JSON is rejected loudly: the CLI prints `Error: invalid trace context
JSON` to stderr and exits `1` without running the subcommand. An empty value is
treated as absent.

## `skill-ledger rotate-keys`

Visible in ordinary help and marked as not implemented. Invoking it fails
without touching any key material.

| | Behaviour |
|---|-----------|
| stdout | Empty |
| stderr | `Error: rotate-keys is not implemented; no keys were changed.` |
| Exit code | `1` |
| Key store | `key.enc`, `key.pub`, and the keyring are left unchanged |

`rotate-keys --help` exits `0` and reports that the command is not implemented.

`init --force-keys` forces a fresh key pair and archives the previous public key
for verification of historical signatures. It does not implement the complete
rotation workflow reserved by `rotate-keys`.
