# Policy CLI User Guide

[中文版](../../../zh/agent-security/agent-sec-core/policy-cli.md)

Use `agent-sec-cli` to manage revisioned Policy Templates and immutable Scope
assignments through `asc-daemon`. Each Scope saves the selected Policy revision's
complete content. The daemon discovers matching process instances, creates their
Bindings, and reconciles deployment automatically. Binding commands are read-only.

These commands are available in the V2 CLI; the released Python CLI does not yet
include them. Policy, Scope and Binding state persists in SQLite across daemon restarts. Scope acceptance does not
mean protection has taken effect. Upgrade CLI and daemon together: Scope update,
Scope revision arguments and manual Binding mutations have been removed.

## Connect to the daemon

Use the absolute socket path supplied by your deployment administrator. The examples
below assume `SOCKET` contains that path and your user is authorized to administer Policies.
An unauthorized caller receives `permission_denied`. The CLI does not start the daemon
or grant permissions.

```bash
agent-sec-cli --socket "$SOCKET" policy list
```

## Correlate local logs

V2 uses native OpenTelemetry for local request correlation. `--trace-context`
retains the existing flat Agent metadata input; place it before command names and
other options' non-option values. Optional `--otel-context` accepts a version 1
JSON carrier with `traceparent`, `tracestate` and `baggage`. The explicit flat
Agent fields take precedence when both are supplied.

```bash
RUST_LOG=info agent-sec-cli --trace-context '{"session_id":"session-123","agent_name":"openclaw"}' \
  --otel-context '{"version":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}' \
  --socket "$SOCKET" policy list
```

`RUST_LOG=info` enables bounded JSON correlation diagnostics on stderr; configure
the daemon environment separately to see its records. Default warn leaves these
records disabled. Diagnostics may be dropped under back-pressure; command results
and errors retain their existing output and exit-code semantics.
No public OTLP exporter or exporter/sampling/batch configuration is provided.
`OTEL_*` settings cannot enable export or change the fixed local sampling policy.
`--otel-context` is incoming context, not an export destination.
The CLI requires the new daemon with carrier support; upgrade the daemon first.

## Manage Policies

Create a JSON template file such as `policy.json`:

```json
{
  "specVersion": "0.1",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": {"type": "file", "path": "/workspace/important/**"},
      "where": {"operation": {"eq": "delete"}},
      "because": "Protect important files from deletion"
    }
  ]
}
```

A PolicyTemplate is a reusable policy that multiple Scopes can select by ID/revision.
Its `rules` describe actions, typed targets, decisions, conditions and optional reasons.
The current resource format is `{"type":"file","path":"..."}`; use one rule per path.
File read/write/exec rules, logical conditions, history predicates, and block/allow/
require_confirmation decisions can be saved when valid. Network and AgentHook target
formats are not yet defined and cannot be saved.

The AgentSight Adapter currently executes only block + file/write + operation=delete
rules without history. Unsupported rules fail the entire Binding with a bounded code
such as `RULE_1_UNSUPPORTED_EFFECT`; no supported subset is sent. Quotes, backslashes
and control characters in `because` are rejected during translation because the target
DSL has no escape syntax; ordinary Unicode text is preserved. An omitted reason uses
the Adapter's default. Template creation success does not prove backend support.

The generated DSL expresses deletion prevention. Actual delete-only kernel behavior
requires separate validation; the current ActPlane backend shares its unlink/write
operation mapping. Old fixed-kind JSON and development databases containing it are
not migrated or automatically rebuilt; clean up old deployments with the compatible
binary before arranging a fresh database.

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

Use the ID and revision returned by successful commands. These are reference
examples, not a sequential script. Create a Scope while the desired Policy revision
is current. Both create and update require a name and complete template; update is
not a partial patch. A changed name or template increments the revision; identical
content does not. Only the current complete record is retained. Policy get/delete
require its exact revision; an old revision returns `not_found`. File paths are
resolved relative to the CLI working directory.

Updating or deleting a Policy does not change existing Scopes or their Bindings.
Delete the Scope to withdraw its assignments. A known deleted Policy ID retains its
revision counter; updating it recreates content at the next revision. An unknown ID
cannot be updated.

## Manage Scopes

Create an assignment with a Policy ID and exact revision, plus one selector:
`--process-name` for an exact Linux process name (1–15 bytes), `--executable` for an
exact normalized absolute executable path, or `--pid` for one process instance.
Name and path selectors discover existing and future matching processes. PID
selection pins the first observed instance and never follows PID reuse. Cgroup
assignments are currently rejected.

```bash
agent-sec-cli --socket "$SOCKET" scope create --process-name openclaw --policy-id "$POLICY_ID" --policy-revision 1
agent-sec-cli --socket "$SOCKET" scope get --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" scope delete --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope retry --scope-id "$SCOPE_ID"
```

Scope has no revision and cannot be updated. Its `policySnapshots` contain the
complete selected Policies. If a concurrent Policy update or deletion wins before
Scope admission, creation fails with `not_found`; it never selects another revision
silently. Once admitted, the saved snapshot also applies to future matching instances.
The CLI assigns one Policy; the RPC accepts 1–32 distinct Policies per Scope.

To replace an assignment, create a new Scope, inspect its Bindings until the required
instances are `READY`, then delete the old Scope. This is not an atomic switch.

Deletion closes discovery admission, stops its worker and asynchronously removes
owned deployments. The response is `{"scopeId":"...","completed":false}` while
cleanup remains, or `completed:true` after removal. Until then the Scope remains
`DELETING`, with failed Bindings visible. Repeating delete preserves retry budgets.
Use `scope retry` to retry terminal failures after resolving their cause; it does not
change the assignment or restart work that is already running. Completed deletion and deletion of unknown Scope IDs both return `completed:true`,
including after daemon restart. Other Scopes and the source Policy are unaffected.

## Inspect Bindings

The daemon creates one Binding per Scope, Policy and process instance. Each record
contains its Policy snapshot, Scope ID and selector, pinned process identity, and
current status. Query `spec.scope.scopeId` to identify the owning assignment.

```bash
agent-sec-cli --socket "$SOCKET" binding list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" binding get --binding-id "$BINDING_ID"
```

Application progresses from `PENDING_APPLY` through `APPLYING` to `READY`.
Permanent or exhausted failures appear as `APPLY_FAILED` or `DELETE_FAILED` with
`status.error`; query commands still exit successfully. Process exit or selector
mismatch retires the Binding through the same cleanup path as Scope deletion.
Cleanup retains uncertain deployment outcomes until absence is confirmed. Automatic
retries are bounded; `scope retry` retries failed owned work. There is no `--wait`.

## Common options

| Option | Purpose |
|--------|---------|
| `--socket PATH` | Required absolute daemon socket path; accepted before or after the subcommand |
| `--timeout-ms N` | Positive unsigned 32-bit integer, default `5000`; connection, sending and receiving share one time budget |
| `--limit N` | List page size, `1..=1000`; default `100` |
| `--offset N` | List offset, unsigned 32-bit integer; default `0` |
| `--help` | Help at the root, command group or operation level; no daemon connection |
| `--version` | Root-level version output; no daemon connection |

Options accept `--key value` and `--key=value`. Quote names and paths containing spaces;
for a value beginning with `--`, use `--key=--value`. Repeated options are rejected.
List commands return one `{items,total}` page. `total` is the count before pagination;
request subsequent pages explicitly, advancing offset by the actual number of returned
items. Pages may stop early at the 3 MiB encoded item budget.

## Output and errors

| Result | Output | Exit status |
|--------|--------|-------------|
| Success | Method result as formatted JSON on stdout; stderr is empty | `0` |
| Daemon rejection | JSON `{requestId,error:{code,message}}` on stderr; stdout is empty | `1` |
| File, connection, response or output failure | Error explanation on stderr | `1` |
| Invalid command-line arguments | Usage error on stderr | `2` |

Template files are limited to 4 MiB and must contain valid JSON without unknown or duplicate
fields. The complete encoded request and response each have a separate 4 MiB limit, including
the line delimiter. Passing the file-size check does not guarantee the assembled request fits;
an oversized request is rejected before sending. The daemon additionally rejects a
serialized Policy or complete Scope larger than 1 MiB before storing it.

The timeout covers daemon communication, including waiting for daemon processing. It excludes
file reading, request encoding, response decoding and output. The CLI never retries automatically.
A timeout or invalid/missing response after sending does not prove the request was not executed;
inspect current state before submitting another change.
