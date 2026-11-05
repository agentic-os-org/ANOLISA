# Connect application-owned instances

[中文版](../../../zh/aw/deployment/independent-instances.md)

`aw instance` lets an explicit Adapter prepare an instance on an already running,
same-user Linux daemon, acknowledge its native wiring, recover its registration
state and release its resources. The application retains process ownership.
The first contract supports the ordinary `qoder-sdk` / `sdk-worker` and
`qwenpaw` / `app` route projections with fixed configuration. Language packages
and native factory migration are separate delivery work; existing factories
continue to use their documented `aw run` entrypoints.

## Connect and select a generation

Start the service separately with the application's configuration. The selected
`spec.agents` target must declare the matching Adapter. Use the exact socket
reported by the service:

```bash
AW_SOCKET=/absolute/private/state/aw.sock
aw instance --socket "$AW_SOCKET" --connect
```

Discovery checks the same-user endpoint, service protocol and registration
support. It returns `api_version: aw-instance/v1`, `service_protocol`, `identity`
and `result.connected: true`. Retain the complete `identity` (generation and
bootstrap configuration digest) for subsequent calls. The command never starts
a daemon, reloads configuration or executes the target's `argv`.

```bash
aw instance --socket "$AW_SOCKET" --timeout-ms 10000 < request.json
```

Every action reads one JSON document, limited to 1 MiB, from stdin. The default
budget is 30,000 ms; `--timeout-ms` accepts 1–60,000 ms and covers stdin,
connection and the operation. Pass an argument array from the application and
close stdin after writing. Syntax errors use the normal CLI stderr/exit-1
contract; admitted commands always emit the JSON envelope below unless stdout
itself fails.

## Request and response

A registration request has this shape. Replace the example identity, PID and
start time with live values; `target` selects the configuration key.

```json
{
  "api_version": "aw-instance/v1",
  "identity": {"generation": "DISCOVERED_GENERATION", "config_revision": "DISCOVERED_DIGEST"},
  "command": {
    "action": "register",
    "key": {"request_id": "application-instance-1", "owner": {"pid": 1234, "start_ticks": "5678"}},
    "target": "target",
    "capabilities": {
      "adapter": "qoder-sdk", "version": "1.0.56", "entrypoint": "sdk-worker",
      "events": {"tool.before": ["observe", "block"], "tool.after": ["observe"]},
      "failure_actions": {}
    },
    "cwd": "/absolute/application/workspace",
    "environment": {"PATH": "/usr/bin:/bin"}
  }
}
```

The Adapter supplies its verified native version, entrypoint and actual supported
events/effects; the sample capability subset is not a substitute for native
verification. Provider cwd must be absolute. Environment is explicit and never
inherited from the daemon; include the application's required values. These
values are trusted application context, not a `/proc/environ` snapshot or proof
of native interception. Provider preparation remains specific to this target and
context even when another instance has the same configuration digest.

`owner.pid` identifies the application process. `owner.start_ticks` is field 22
of `/proc/PID/stat`, encoded as a string. The daemon pins UID, start time and PID
namespace and pins the Unix peer identity before reading the request: the owner may call directly, or spawn
the CLI as its immediate child. Shell/proxy intermediaries are rejected. The
owner must remain alive through preparation and confirmation. Other processes
cannot claim ownership just by submitting its PID.

All actions use the same outer `api_version` and `identity`. Their `command`
objects are:

| Action | Required command fields |
| --- | --- |
| `register` | `key`, `target`, `capabilities`, `cwd`, `environment` |
| `query` | `key` from the original request |
| `ready` | Complete returned `handle`, returned `policy_revision` |
| `release` | Complete returned `handle` |

`key.request_id` is 1–128 ASCII letters, digits, `-`, `_`, `.`, or `:`. Generate a
new ID for each intended instance. The returned handle contains the key and the
complete binding: daemon identity, instance ID, target and preparation audit key.
Do not edit these fields or combine parts of two handles.

Every response contains `api_version`, `service_protocol`, `identity`, `result`,
`error` and `audit_key`. A successful operation has a non-null `result`, null
`error` and exit 0. A rejected operation has null `result`, a stable string
`error` and exit 1. A failed preparation can also carry its durable `audit_key`.
`identity` is null when input or connection failed before discovery.

## Ready and task scopes

Registration returns `state: pending_ready`, `handle`, `policy_revision`,
`routes` (`events` and native `hooks`), `confirmation_timeout_ms: 30000` and the
remaining confirmation time. The prepared root cannot execute Hook events.
Install the returned routes at the actual native entrypoint, then send `ready`
with the same handle and revision. Ready records an authenticated Adapter
acknowledgement. The Adapter owns verification that those callbacks are installed
and their results are adopted; Service cannot infer that from a PID.

A Ready instance admits the existing task-scope protocol. The worker is the owner
or one of its descendants; each task operation must originate from that exact
worker or its direct CLI child. Use `open_scope` with the root ID and worker
context, retain the returned task binding, and acknowledge it within five seconds.
`scope_policy` also returns the fixed task's native routes. Existing `aw scope`
operations support policy, acknowledgement, invocation registration, native
callbacks, validity checks and close. A CLI HookBinding uses the returned root
binding, socket, Adapter, cwd and `routes.events`, stored in an application-owned
mode-0600 file. See the runnable fixture below for the complete sequence.

Keep one task scope for the whole request, including model waits, native tools,
callbacks and cancellation cleanup. Stop admission and drain original native work
before closing its task scope or releasing the root. Release cancels and joins
AW Provider work and descendants; it does not stop the application, native Worker,
sibling instance or daemon. An explicit release timeout leaves cleanup pending;
query the state and retry release after native cleanup. Never dispatch a tool
because a lifecycle call failed or timed out.

## Recovery and errors

| State | Meaning and next action |
| --- | --- |
| `preparing` | The original registration is still running; query again |
| `pending_ready` | Resources prepared; install routes and confirm before the remaining deadline |
| `ready` | Ready acknowledged; tasks may be opened |
| `closing` | Admission fenced; cancellation/cleanup is still pending |
| `released` | The instance's resources have been reclaimed |
| `expired` | Unconfirmed preparation was reclaimed; use a new request ID for an explicit new attempt |
| `failed` | Preparation failed; inspect nested `error` and `audit_key` |

A lost response is an unknown result: query with the original key and identity.
Identical repeated registration returns the retained state without preparing
again or extending the confirmation window. Changed input under the same key
returns `registration_request_conflict`. Successful queries and duplicate
registrations can return terminal `failed` states; inspect `result.state` even
when exit is zero. Repeated Ready and release are idempotent. Pending instances
expire 30 seconds after preparation; owner exit triggers automatic cleanup.

Replay records remain for the daemon generation, with a 4,096-key capacity.
Exhaustion rejects new keys with `registration_limit`; existing keys remain
queryable. Plan a drained daemon restart when this capacity is reached. Restart
invalidates every old handle. Discover the new identity and explicitly register
a new instance; do not replay original tools or silently reuse old readiness.

| Error | Handling |
| --- | --- |
| `stale_service`, `protocol_version`, `registration_protocol_version`, `registration_unsupported` | Check the selected daemon/client combination; reconnect explicitly |
| `registration_input_invalid`, `registration_input_unavailable` | Correct JSON or finish stdin within the deadline |
| `registration_request_id`, `registration_request_conflict` | Correct the key or use a new ID for an intentionally different instance |
| `registration_owner_mismatch`, `registration_peer_mismatch`, `stale_process`, `bridge_process_mismatch` | Fix process/start identity or direct-caller/worker relationship |
| `registration_handle_mismatch`, `registration_unknown` | Use the exact returned handle or original key; unknown is not Ready |
| `registration_not_ready`, `registration_confirmation_expired`, `registration_unavailable` | Query; finish confirmation or explicitly start a new attempt after cleanup |
| `registration_policy_mismatch` | Install the returned revision before Ready |
| `registration_requires_scope` | Execute through a task scope; use registration release for a root |
| `binding_preflight`, `registration_adapter_unsupported`, `registration_cwd` | Fix target, capabilities, native routing constraints or absolute cwd |
| `binding_limit`, `registration_limit`, `connection_limit` | Release/drain resources or follow the replay-capacity procedure above |
| `instance_release_timeout`, `transport_unavailable` | Treat outcome as unknown/pending; query before further work |
| `audit_unavailable`, `service_failure` | Stop dispatch and diagnose service/audit failure |

Provider failures preserve their existing preparation audit and runtime failure
actions. An audit failure retains the service's fail-stop behavior.

## Runnable contract fixture

From a Linux checkout with Rust, Python and Node prerequisites installed:

```bash
cd src/aw
cargo test --locked -p aw-service --lib runtime::registration
cargo test --locked -p aw-cli --test service registration::
```

The [application process fixture](../../../../../src/aw/crates/aw-cli/tests/fixtures/independent_owner.py)
runs as an ordinary Python subprocess and invokes the real CLI. It verifies
registration, Ready gating, task callbacks, actual fixture side effects, two
instances in one PID, two owners, independent release and owner-loss cleanup.
Additional socket tests cover a discarded response, Provider preparation failure
and cancellation of in-flight Provider descendants. Service unit tests advance
the handoff deadline deterministically and check audit failure.
These fixtures certify the independent lifecycle contract with simulated native
boundaries. Real Qoder/QwenPaw factory wiring and business adoption remain separate
acceptance. To roll back, drain and release independent instances and restore the
previous AW binary and application entrypoint together.
