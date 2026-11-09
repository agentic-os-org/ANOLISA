# Connect application-owned instances

[中文版](../../../zh/aw/deployment/independent-instances.md)

`aw instance` lets applications keep their native startup command while AW owns
policy preparation, native readiness and resource cleanup. The application keeps
process ownership. An Adapter connects the supported native entrypoint and
confirms its callbacks before the instance becomes Ready.

## Prepare native startup

Use the same `aw.yaml` for the service, native startup and later updates. The
configuration selects the existing executable and native arguments; setup does
not turn an arbitrary running process into a managed instance.

| Application | Initial connection | Runtime ownership |
| --- | --- | --- |
| Qoder SDK, AgentScope, Pi and Claude SDK | Application calls `aw instance prepare --agent TARGET` as a direct child, applies its returned environment and enters the documented AW factory in the same process | Each native task/session uses the existing child Scope contract |
| QwenPaw App | `aw instance setup` installs a startup component in its dedicated venv; restart the native App once | Existing resident plugin and reply Scopes |
| OpenClaw Gateway and Hermes local chat | `aw instance setup` installs startup wiring; restart the native command once | Root native bridge reserves complete runs/turns and tracks callback cleanup |
| Qoder CLI | `aw instance setup` installs native startup Hooks for subsequent TUI or print launches; existing TUI can use [pre-request attachment](process-attach.md) | Each process registers its own root bridge at `SessionStart` |

Follow the [Agent guide](../native-agents.md) for the framework's bootstrap,
settings and accepted arguments. `setup` writes only its returned file manifest
and reports the startup environment. It creates no running instance. `prepare`
performs Adapter validation and registration; its response contains
`instance_id`, changed `environment`, private `binding`, the prepared `command`
and `state: pending_ready`. The SDK application or startup module uses those
assets and confirms the original owner process at the native readiness boundary.
Do not launch the returned command as an unrelated replacement process.

After readiness, pause new work, edit policy fields in `aw.yaml`, and update:

```bash
aw instance update --agent TARGET
aw status --config aw.yaml
```

A later preparation from the same configuration path reuses the selected daemon
and validates the edited policy against its startup configuration. The daemon
keeps its original generation and bootstrap digest; each instance has a separate
active policy revision. Native inputs, sessions, callbacks and cleanup must drain
before activation. See [policy updates](policy-updates.md) for each framework's
boundary and restrictions.

The lower-level JSON protocol below remains available to explicit integrations.
The supplied startup code owns its calls; ordinary applications can use the
framework bootstrap without constructing handles or private lifecycle RPCs.

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
| `register` | `key`, `target`, `capabilities`, `cwd`, `environment`; optional `config` |
| `register_native` | Same fields; only the admitted Qoder/OpenClaw/Hermes root bridge |
| `query` | `key` from the original request |
| `ready` | Complete returned `handle`, returned `policy_revision` |
| `release` | Complete returned `handle` |

`register` projects ordinary Qoder SDK, AgentScope, Pi, Claude SDK and QwenPaw
factory routes into child Scopes. `register_native` selects Qoder/OpenClaw/Hermes root
callbacks with their stable request and tool lifecycle. Optional `config` is the
complete YAML or JSON configuration as a string; only Providers, events and the
default event budget may differ from the daemon's startup configuration. Omitting
it uses that startup configuration. A candidate changes the instance policy
revision while the binding retains the daemon's original identity.

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
remaining confirmation time. A pending root cannot execute Hook events.
Install the returned routes at the actual native entrypoint, then send `ready`
with the same handle and revision. Ready records an authenticated Adapter
acknowledgement. The Adapter owns verification that those callbacks are installed
and their results are adopted; Service cannot infer that from a PID.

A Ready ordinary factory root admits the existing task-scope protocol; its Hook
execution still requires a child Scope. The worker is the owner
or one of its descendants; each task operation must originate from that exact
worker or its direct CLI child. Use `open_scope` with the root ID and worker
context, retain the returned task binding, and acknowledge it within five seconds.
`scope_policy` also returns the fixed task's native routes. Existing `aw scope`
operations support policy, acknowledgement, invocation registration, native
callbacks, validity checks and close. A CLI HookBinding uses the returned root
binding, socket, Adapter, cwd and `routes.events`, stored in an application-owned
mode-0600 file. See the runnable fixture below for the complete sequence.

The native root mode uses its admitted stable bridge instead: the original owner
confirms readiness and process association, then the bridge reserves each native
run or turn before work and releases it only after confirmed cleanup. Native
callback peers must belong to the owner's process tree. Pending roots and
unrelated peers cannot execute callbacks. Root mode is restricted to the admitted
Qoder/OpenClaw/Hermes entrypoints; it does not grant ordinary SDK roots direct execution.

Keep one task scope for the whole request, including model waits, native tools,
callbacks and cancellation cleanup. Stop admission and drain original native work
before closing its task scope or releasing the root. Release cancels and joins
AW Provider work and descendants; it does not stop the application, native Worker,
sibling instance or daemon. An explicit release timeout leaves cleanup pending;
query the state and retry release after native cleanup. Never dispatch a tool
because a lifecycle call failed or timed out.

Optional evidence starts after native readiness or task-Scope acknowledgement.
Each Scope retains its own bounded observation worker and source-session claims;
closing it drains those reads before releasing the claims. Native startup keeps
the Adapter's existing source-association coverage, including unverified or
unavailable sources. Evidence configuration is fixed for the instance lifetime;
change it through preparation and restart.

## Recovery and errors

| State | Meaning and next action |
| --- | --- |
| `preparing` | The original registration is still running; query again |
| `pending_ready` | Resources prepared; install routes and confirm before the remaining deadline |
| `ready` | Ready acknowledged; the admitted task or native bridge may accept work |
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
boundaries. Framework-specific startup and native acceptance are tracked in the
[native matrix](../../../../../src/aw/tests/native-matrix.json); their versions and
limits remain separate from the registration contract.

## Cleanup and rollback

The application stops dispatch, closes its native factory/tasks and invokes
`aw instance release --binding FILE` directly while the owner is alive. Successful
release drains its AW resources and removes the exact private `native-*` directory
returned during preparation. Owner and direct-child checks still apply; another
controller cannot release the binding by copying its path.

After a crash, owner-loss handling reclaims Service resources, but local staged
files may remain. Verify the owner identity has exited and its runtime resources
have drained before removing only that preparation's recorded directory. A dead
owner's binding cannot be reused to authenticate `release` or a new process.
Service restart also invalidates old handles.

To remove persistent startup wiring, stop affected native applications, remove
only the files returned by `setup`, and restore the application's prior startup
environment. Preserve unrelated `NODE_OPTIONS`, Python startup files and native
settings. Follow the Agent guide for any separately connected plugin cleanup,
then restart normally. SDK rollback removes its bootstrap together with the AW
factory integration after draining work. Restore matching AW and application
versions together when reverting the integration.
