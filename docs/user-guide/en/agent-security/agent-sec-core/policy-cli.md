# Policy CLI User Guide

[中文版](../../../zh/agent-security/agent-sec-core/policy-cli.md)

Use `agent-sec-cli` to create, inspect, update and delete Policies, Scopes and Bindings
through `asc-daemon`. A Policy describes the protection, a Scope selects its target,
and a Binding associates a Policy revision with a Scope revision.

These commands are available in the V2 CLI; the released Python CLI does not yet
include them. Current Policy records are lost when the daemon restarts, and accepting
a Binding does not mean protection has taken effect.

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
{"kind":"prevent_file_deletion","files":["/workspace/important/**"]}
```

The currently supported template protects against file/directory entry deletion
(`unlink`/`rmdir`); it does not cover renaming, moving or modifying file contents.

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

Use the ID and revision returned by successful commands. These are reference examples,
not a sequential script: retain a Policy and Scope while creating a Binding that references them.
Both create and update require a name and a complete template. Update replaces the existing
Policy; it is not a partial update and cannot create a missing Policy. File paths are resolved
relative to the CLI working directory. Policy get/delete require the exact current revision;
an old revision returns `not_found`.

## Manage Scopes

Select exactly one of `--pid` (a positive process ID) or `--cgroup-id` (a positive cgroup ID).

```bash
agent-sec-cli --socket "$SOCKET" scope create --pid 4242
agent-sec-cli --socket "$SOCKET" scope get --scope-id "$SCOPE_ID" --revision 1
agent-sec-cli --socket "$SOCKET" scope list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" scope update --scope-id "$SCOPE_ID" --cgroup-id 99
agent-sec-cli --socket "$SOCKET" scope delete --scope-id "$SCOPE_ID" --revision 2
```

Scope update replaces the complete selector. Get/delete require the exact current revision.
Process IDs and revisions are positive 32-bit unsigned integers; cgroup IDs are positive
64-bit unsigned integers.

## Manage Bindings

A Binding references an existing Policy revision and Scope revision. All create commands
return server-generated IDs. Binding get/delete require the Binding ID, without a revision.

```bash
agent-sec-cli --socket "$SOCKET" binding create --policy-id "$POLICY_ID" --policy-revision 1 --scope-id "$SCOPE_ID" --scope-revision 1
agent-sec-cli --socket "$SOCKET" binding get --binding-id "$BINDING_ID"
agent-sec-cli --socket "$SOCKET" binding list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" binding update --binding-id "$BINDING_ID" --policy-id "$POLICY_ID" --policy-revision 2 --scope-id "$SCOPE_ID" --scope-revision 2
agent-sec-cli --socket "$SOCKET" binding delete --binding-id "$BINDING_ID"
```

Create/update normally return `PENDING_APPLY`; delete requests `PENDING_DELETE`.
Repeated or unchanged operations can retain the existing status and revision. These statuses
record the requested change, not completed protection or deletion. Automatic application and
`--wait` are not currently available.

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
request subsequent pages explicitly.

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
an oversized request is rejected before sending.

The timeout covers daemon communication, including waiting for daemon processing. It excludes
file reading, request encoding, response decoding and output. The CLI never retries automatically.
A timeout or invalid/missing response after sending does not prove the request was not executed;
inspect current state before submitting another change.
