# CLI automation reference

[中文版](../../../zh/aw/reference/cli.md)

For the full command and option inventory, see the user [CLI reference](../../../../user-guide/en/aw/cli-reference.md).
This page describes output contracts for scripts and automation.

Use `aw commands --json` for command and argument discovery, and command-specific `--help` for conditional requirements.

## Output selection

| Entrypoint | Output |
| --- | --- |
| `install`, `uninstall`, `list --installed`, `doctor`, `recover`, `config init/validate/add-*/remove-*` | Optional local `--json` |
| `service status/stop`, `connect` for a live application, `disconnect`, `instance`, `request`, `config show`, `update` | Always JSON; do not add `--json` |
| `connect --agent TARGET` without a PID | Use the exit status for persistent Hook wiring. An Adapter may print setup instructions. |
| `inspect` | Requires `--json` |
| `run` | Preserves native Agent output; forwards arguments after `--` unchanged |

Human management output accepts `--quiet`, `--verbose` and `--color auto|always|never`.
Quiet and verbose are mutually exclusive; all three conflict with JSON. Quiet retains errors and cleanup warnings.

## Management reports

```bash
aw config validate --config "$HOME/aw/aw.yaml" --json
aw doctor --prefix "$AW_PREFIX" --json
```

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

| Field or status | Consumer action |
| --- | --- |
| `format: 1` | Report version |
| `status: succeeded` | Exit 0; read typed `result` |
| `failed` / `cancelled` / `partial` | Exit 1; read `error.code`; partial means incomplete recovery |
| `mutation.committed` | Whether package files and records crossed the durable commit point |
| `mutation.recovery` | `not_attempted`, `complete` or `incomplete` |
| `warnings` | Inspect resource paths and finish cleanup; committed uninstall may exit 0 with warnings |
| `changed: false` | Editing preserved configuration bytes |
| `policy_update_scope: true` | Candidate passed offline scope validation; run `aw update` to activate it |

Syntax errors print text to stderr, leave stdout empty and exit 1. I/O failure can occur after publication; inspect state before retrying.
`config show` returns private configuration values. Fixed management error summaries do not apply to native output or protocol streams.

## Uninstall and recovery

Scripts use `aw uninstall --prefix ABS_PREFIX --yes`; system scope also requires `sudo` and `--scope system`.
Interactive uninstall accepts only `y` or `yes`; JSON and non-terminal calls require `--yes`. `--dry-run` only previews.
See [package management](packages.md) for `needs_recovery`, `package_in_use`, `plan_blocked` and recovery steps.

## Command discovery

`aw commands --json` returns `format`, `name`, `version` and `commands`, with command paths and visible arguments.
`required` marks unconditional requirements only. Completion uses the same declarations and supports bash, zsh, fish, powershell and elvish.
Help and discovery run offline. `aw -V` reports the CLI crate version.

## Independent instance JSON

`aw instance` uses the `aw-instance/v1` envelope, separating `result.state`, operation errors and preparation audit keys. See [independent instances](../deployment/independent-instances.md) for fields, confirmation deadlines and duplicate-request handling.

## Public audit export

`aw audit-export` reads one page from an existing same-user service state directory.
The service may be stopped. Use its configured state path; do not create an empty
replacement. The journal directory must be owned by the effective user with mode
0700, its source descriptor and chain files with mode 0600. The request file is
bounded to 16 KiB and must be regular. The CLI accepts no control operation.

```bash
AW_STATE=/absolute/private/state
AW_QUERY="$HOME/aw-audit-query.json"
cat > "$AW_QUERY" <<'JSON'
{"api_version":"audit-export.aw/v1alpha1","operation":{"type":"enumerate","cursor":null,"limit":64}}
JSON
aw audit-export --state-dir "$AW_STATE" --request "$AW_QUERY"
```

A successful response has `error: null`, a durable `source_id`, and `result.keys`.
Copy `result.next_cursor` into the next enumeration request until it is null.
`result.changed` reports key-set changes while continuation still progresses.
The cursor retains the first page's `through` ceiling; a new scan collects later
and newly earlier keys. Retain per-chain checkpoints and bound the consumer loop.
For each key, send `{"type":"read","chain_id":"KEY","cursor":null,"limit":64}`
as `operation`, retaining the same `api_version` wrapper. Replace `KEY` with a
returned key. Continue with that response's `result.cursor` in the next read.

Read responses contain the ExecutionEvent, public AuditRecords, captured tip,
`caught_up`, `terminal` and fixed limits. A page counts source positions, so it can
have no public records and still advance. Only `caught_up && terminal` completes
a chain; revisit open chains and periodically start a new inventory scan.
Persist records and cursors atomically, deduplicate by producer and UID, and
compare object digests on replay. Historical producer generations remain intact.
The response never contains private journal records. See the
[export contract](../../../../../src/aw/docs/design/en/runtime/audit-export.md)
for schema names, bounds, corruption and missing-history handling.

Protocol failures return `result: null` and a stable `error` code, with exit 1.
Command syntax/request-file I/O failures use stderr. Cancellation has no write
side effects. After the one-shot command exits there is no resource to stop.
Remove `"$AW_QUERY"` when the request file is no longer needed.

`aw instance list`, `aw connect --sdk-instance ID` and `aw disconnect --sdk-instance ID`
operate on application-installed SDK listeners. `aw connect --sdk-pid PID` selects
one listener by its pinned process identity; use the instance ID when a process
has multiple listeners. With `--config` and `daemon.startup: on_demand`, this PID
selector starts or reuses AW. With `--socket`, it requires an existing service.
It leaves application startup and ownership native. The existing `--pid`
selector retains its native hotload restrictions. Inspect confirmation in JSON. See
[cooperative SDK control](../deployment/native-sdk.md#cooperative-attach-and-detach).

## Manual RPC completion audit

A same-user integration can report a completed tool through `aw request` without
constructing a framework callback. For `open_event`, supply the existing
`event.tool.result_projection.status` when a text projection is available, or the
optional operation-level `result_status` for a result without that projection.
Both accept `success`, `error`, `interrupted`, or `denied`. When supplied together,
they must agree. When `native` identifies a supported completion callback, its
recognized status must also agree. The operation-level `result_status` requires
a `tool.after` event with a valid public tool-call identity. A request using only
the existing projection can still execute Providers without a public tool-call
ID, with `projection_unavailable` in audit export.

Qoder, Qoder SDK and Claude SDK events with native `hook_event_name: PostToolUse`
can also report success using a Boolean `tool.result.isError: false` or
`tool.result.is_error: false`, without duplicating the result in
`native.tool_response`. Explicit failure or interruption takes precedence.
Missing flags, string values such as `"false"` and arbitrary business fields do
not establish success. This native-dialect mapping applies to manual RPC and
native callbacks alike.

These are caller-reported facts: ordinary RPC association remains unverified.
The service stores `resultStatus` and a digest of the original `tool.result` when
opening the event. Provider success and replacement text do not change that tool
outcome or digest. For framework-independent reports, send `native: {}` and put
arbitrary JSON in `tool.result`. The reported status applies even when the
result contains fields such as `isError` or `state` for business use. Providers
receive the normalized event with its original result.

Use a CLI and daemon containing this RPC completion support. Older daemons reject
the new operation field. JSON callers can omit it. Rust callers constructing
`aw_service::Operation::OpenEvent` set `result_status: None` to omit it. Other
event kinds retain their existing projection behavior when the field is omitted.
An event without a recognized native status or an applicable result projection
can still produce `projection_unavailable`. Existing historical gap chains are unchanged.

For an integration check, start a private test service with a configured target
and a `tool.after` Provider step, for example Tokenless `post_tool` from the
[Provider integration guide](../providers/tokenless.md). Run from any directory
as the service owner, with `aw` on PATH and Python 3 installed. Set the three
variables below to that service's state directory, target name and adapter.
The example reports a synthetic completion to exercise the audit API; it does
not launch an Agent or execute the command described by the event.

```bash
export AW_STATE=/absolute/private/state
export AW_TARGET=openclaw
export AW_ADAPTER=openclaw
python3 - <<'PYTHON'
import json
import os
from pathlib import Path
import subprocess
import tempfile
import uuid

state = Path(os.environ["AW_STATE"])
target = os.environ["AW_TARGET"]
adapter = os.environ["AW_ADAPTER"]

def rpc(operation):
    reply = subprocess.run(
        ["aw", "request", "--socket", str(state / "aw.sock")],
        input=json.dumps(operation), text=True, capture_output=True,
        check=True, timeout=10,
    )
    return json.loads(reply.stdout)["result"]

binding = rpc({
    "method": "bind", "target": target, "cwd": str(Path.cwd()), "environment": {},
    "capabilities": {
        "adapter": adapter, "version": "audit-example", "entrypoint": "manual-rpc",
        "events": {"tool.before": ["observe", "block"],
                   "tool.after": ["observe", "replace_result"]},
    },
})
instance = binding["instance_id"]
try:
    handle = rpc({
        "method": "open_event", "instance_id": instance, "result_status": "success",
        "event": {
            "name": "tool.after",
            "agent": {"adapter": adapter, "binding_id": target, "instance_id": instance},
            "session_id": "audit-example",
            "tool": {
                "name": "exec", "native_name": "exec", "call_id": str(uuid.uuid4()),
                "input": {"command": "printf AW_AUDIT_EXAMPLE"},
                "result": {"stdout": "AW_AUDIT_EXAMPLE", "exitCode": 0},
                "result_projection": {"text": "AW_AUDIT_EXAMPLE", "status": "success",
                    "content_origin": "command_output", "command": "printf AW_AUDIT_EXAMPLE"},
            },
            "native": {},
        },
    })
    try:
        for step in handle["steps"]:
            print(json.dumps(rpc({"method": "invoke_step", "instance_id": instance,
                "event_id": handle["event_id"], "step_id": step})))
    finally:
        rpc({"method": "close_event", "instance_id": instance, "event_id": handle["event_id"]})
    with tempfile.TemporaryDirectory() as directory:
        request = Path(directory) / "read.json"
        request.write_text(json.dumps({"api_version": "audit-export.aw/v1alpha1",
            "operation": {"type": "read", "chain_id": handle["event_id"],
                          "cursor": None, "limit": 64}}))
        subprocess.run(["aw", "audit-export", "--state-dir", str(state),
                        "--request", str(request)], check=True, timeout=10)
finally:
    rpc({"method": "release_instance", "instance_id": instance})
PYTHON
```

The read response has `error: null`, an `ExecutionEvent` with
`details.resultStatus: success` and `details.resultDigest`, and a
`provider.completed` record for each invoked Provider. A successful Tokenless
attempt has `providerId: tokenless` and `status: ok` when configured under that
provider ID. Provider failures remain visible with `status: failed` independently
of the tool outcome. Follow `result.cursor` if `caught_up` is false. Closing the
manual event does not establish native effect adoption; `event.closed` retains
its existing `incomplete` outcome. The example releases its binding and deletes
the temporary request file; the pre-existing service remains running.

Invalid enum values are rejected during request decoding. `result_status_conflict`
means the declared, projected or recognized native statuses disagree.
`result_status_requires_tool_after` means the declaration was attached to another
event kind. `event_invalid` identifies an invalid normalized event or projection.
`public_event_invalid` means the operation-level `result_status` was supplied
with missing or invalid public identity metadata. These rejections do not create
an event journal chain or execute a Provider. Correct the reported metadata
before opening a new event.
