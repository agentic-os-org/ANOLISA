# Local service and client

[中文版](local-service_zh.md)

`aw-service` exposes prepared Provider execution through a standalone Linux
process and a reusable synchronous Rust client. One service owns one immutable
AW configuration snapshot, its live bindings and events, and durable execution
metadata. The `aw run` client starts Qoder CLI 1.1.64 with session-owned native
Hook entries; the daemon is independent of that foreground Agent. Service
execution records do not certify that an Agent adopted a response.

## Component ownership

| Component | Responsibility |
| --- | --- |
| `aw-service::Server` | Private endpoint, configuration identity, binding/event lifetime, bounded RPC handling and audit storage |
| `aw-service::Client` | Versioned requests with one caller deadline and no automatic retries |
| `aw-host` | Provider discovery, configuration validation, admission and once-only step invocation |
| `aw-exec` | Child-process deadlines, output limits, cancellation and process-group cleanup |
| `aw-core::journal::FileJournal` | Durable reservations and verified metadata record chains |
| Qoder Adapter | Verify the supported native version, normalize callbacks, retain native scheduling and apply supported effects |
| `launch_service` | Resolve private paths, verify an existing configuration revision or start an on-demand service |

The service reuses `FileJournal` as storage. It does not run Core plans or produce
Core execution/adoption receipts. Provider-private settings remain in
`spec.providers.<name>.config`; no security-engine rules are embedded in the
service.

## Local protocol

[wire.rs](../../crates/aw-service/src/wire.rs) defines the experimental
`aw-service/v1alpha2` protocol. It is separate from the Provider stdio protocol
`aw-provider/v1alpha1`. Each connection carries one length-framed JSON request
and one response; a frame is limited to 8 MiB. Native stdin and normalized event data each retain
a separate 1 MiB limit; the frame accounts for JSON byte-array expansion.

A request contains `api_version`, `identity`, `deadline_ns` and `operation`.
The initial `status` request may omit identity. Every subsequent request must
match the service's fresh generation and the SHA-256 revision of its original
configuration bytes. Restarting the service invalidates existing clients and
handles, even when the file is unchanged. Editing the file does not reload a
running service.

| `operation.method` | Input beyond the envelope | Result |
| --- | --- | --- |
| `status` | None | PID, resource counts, audit health and unverified adoption status |
| `bind` | `target`, trusted `capabilities`, absolute `cwd`, complete `environment` | Service-issued `instance_id`, identity and preparation `audit_key` |
| `unbind` | `instance_id` | Release a binding after all its events have closed |
| `release_instance` | `instance_id` | Cancel and drain this instance, then release its binding without stopping the shared service |
| `open_event` | `instance_id`, normalized `event` | Event ID, instance ID and admitted step IDs |
| `open_hook_event` | `instance_id`, normalized `event`, exact `native_input` bytes | Shared native event handle; repeated callbacks retain the original deadline |
| `invoke_step` | `event_id`, `instance_id`, `step_id`, optional `native_environment` | Structured candidate effects, explicit native bytes/status, or execution failure with call metadata |
| `close_event` | `event_id`, `instance_id` | Cancel unfinished work, join children and acknowledge closure |
| `audit` | Preparation or event `key` | Verified durable envelopes and a `terminal` flag |
| `stop` | None | Acknowledge requested shutdown; process exit completes cleanup |

`bind` invokes real `describe` and `validate_config` exchanges for structured
Providers; explicit native commands have no such handshake. Capability evidence
comes from the trusted same-user caller, never from Provider claims. Preparation
pins the working directory, explicit environment and configured commands for the
binding. The Qoder launcher also pins `QODER_PROJECT_DIR` and
`CLAUDE_PROJECT_DIR` to that verified directory for command compatibility; it
also reproduces Qoder's source/version/site metadata from the launch snapshot. It
does not copy arbitrary callback environment or per-Hook `env` into Provider
context. Later requests cannot change that context. A preparation failure may return an
`audit_key` without a successful binding; the Rust client exposes it through
`Error::Attempt`.

The environment stays a JSON string map when all entries are UTF-8. Otherwise it
is an array of `[key_bytes, value_bytes]` pairs, each encoded as byte arrays. This
preserves Linux environment bytes through Agent, native Hook and Provider
execution. Empty/duplicate keys, NUL bytes and `=` in keys are rejected.

During binding preparation, connection closure, service shutdown or expiry cancels
the Provider work and releases its slot after cleanup. A failed binding response
write also releases the created instance. `aw run` observes termination while
waiting for this reply; the reusable client exposes `call_cancellable` for callers
with their own cancellation flag. Preparation side effects are not rolled back.

For raw native commands, `invoke_step.native_environment` can carry the complete
callback environment using the same byte-preserving encoding. It replaces, rather
than merges with, the bound environment;
an omitted value retains the bound environment. Structured Providers ignore it.
The snapshot permits at most 4096 entries and 1 MiB, rejects invalid keys and NUL,
and never enters event input, correlation digests or audit. An invalid snapshot
consumes the step claim and produces a visible execution failure without extending
the event deadline. It is an RPC field, not a new `aw.yaml` option.

The current service admits structured `tool.before` with `observe`/`block`,
`tool.after` with `observe`, and explicit native steps at those tool points. It
rejects enabled unsupported events, structured `ask`, result replacement,
final guards and stronger execution guarantees. A successful empty effect list
adds no restriction and grants no native permission. A successful policy block
is distinct from a failed invocation whose `failure_action` is `block`.

Protocol v1alpha2 rejects v1alpha1 daemons during status discovery, before any
binding or callback. Stop old sessions and their daemons using the old binary
before upgrading the CLI and daemon together; never replace a live socket.
This separates the callback-environment wire change from the unchanged
configuration and Provider protocols.

## Shared event lifetime

An Adapter opens an event once, then sends individual `invoke_step` requests in
the order and concurrency required by its native framework. Client clones can
invoke different steps concurrently. The service adds no serial/parallel policy
and does not rerun failed steps. Each step may be attempted once in that event.

The Adapter must share the returned handle across callbacks belonging to the
same native event. A native session or tool-call ID is correlation data, not
proof that two callbacks belong to one service event. `open_hook_event` correlates
nonempty session/tool-call IDs and event name inside a prepared instance, then
requires the same normalized event and exact native bytes for subsequent opens.
It retains closed and failed-open claims until instance release; they cannot
create a new budget. Opening an independent event per step is not valid shared
event scheduling. The current per-instance correlation limit is 1,024 events.

The Qoder Adapter registers one callback per AW step in a shared matcher group.
Qoder retains default parallel execution and its matching-group sequential
behavior. Each callback opens or joins its event, then claims only its own step.
Incomplete events expire even if Qoder never invokes the remaining callbacks.
Current native input is immutable: a sequential command that returns
`updatedInput` can change the next callback stdin, which is rejected as a
correlation mismatch and handled by that step's `on_error`. Supporting rewrite
chains requires a separate contract for per-step input snapshots under the same
deadline; raw output forwarding does not establish that support or portable
approval.

The Linux `CLOCK_MONOTONIC` deadline includes connection setup and framing.
A call may have at most 60 seconds remaining. `open_event` fixes the event deadline
as the earlier of its request deadline and the configured event budget; subsequent
step calls cannot extend it. If an `invoke_step` RPC has a shorter deadline,
expiry of the response wait cancels the whole shared event; executor cleanup
then reaps owned processes. Closing an event cancels unfinished work, joins its
Provider children and writes its terminal record before acknowledging closure.
Expiry also closes abandoned events. The caller must collect required step
results before native tool dispatch.

Each event worker owns its Host reference and cancellation state, then borrows
one `aw_host::Event` on its stack. Bounded channels route requests into scoped
workers. The shared deadline and claims remain in that one Event; no borrowed
Event is stored beside its owning Host in a self-referential registry.

Current resource limits are 16 binding slots, 32 open events, 64 admitted steps
per binding, 64 concurrent step calls and 64 active connections. Exceeding a
limit produces a visible rejection or transport failure. There is no implicit
queue retry. These are service admission limits, not additional configuration
fields.

## Audit and uncertain outcomes

A durable reservation precedes Provider preparation or event execution. Each
step has a start record before invocation and a completion record before the
service returns its effects. Audit write failure prevents a successful effect
response and stops further service processing.

Records contain configuration/generation identity, binding and instance IDs,
selected native correlation fields, step/Provider/request IDs, durations, byte
counts and execution status. They omit tool input/results, private configuration
and raw stdout/stderr. Provider diagnostics and effect reason text are not copied
into the journal.

The preparation key comes from `bind`, including `Error::Attempt` on a recorded
failure. An event ID is also its audit key. `audit` verifies the stored chain and
bounds each queried file to 1 MiB. `terminal: false` means no terminal record is
present: the event may still be active or may have been interrupted. It is not
by itself a crash diagnosis. A closed event does not prove that the native Agent
adopted its effects or executed a tool.

Transport loss or an expired client deadline can leave the caller without a
known result. Do not retry the step or infer permission from that uncertainty.
Inspect its audit record. A newly started service can read old-generation
records, but cannot resume their events, recover their handles or replay them.
Journal digests detect corruption; they are not protection against the same
user rewriting the storage.

## Endpoint, shutdown and restart

The foreground CLI accepts an absolute `--state-dir`. Its existing parent must
be owned by the current user and not writable by its group or others. The service creates the state directory with mode
0700 or requires that mode on an existing directory; it rejects symlinks at these
boundaries rather than changing permissions. The socket is `aw.sock` with mode
0600. An exclusive `service.lock` and same-UID Unix peer credentials protect
service ownership and local access. This is a trusted same-user boundary, not a
sandbox or isolation from other processes of that user.

Explicit `spec.daemon.state_dir` and `spec.daemon.endpoint` values must match the
selected directory and socket. `auto` leaves path selection to the launcher;
`aw serve` still requires `--state-dir`. `aw run` implements on-demand startup;
`external` requires an existing service. With both paths set to `auto`, the
launcher chooses `$XDG_RUNTIME_DIR/aw/<revision-prefix>` or
`/tmp/aw-UID/<revision-prefix>` when the runtime variable is unset. An explicitly
supplied runtime directory must already be private and owned by the current user.
The path uses a shortened revision to fit a Unix socket; reuse still verifies
the entire configuration revision.

Startup holds a private per-state lock and writes an immutable configuration
snapshot. A new service runs in a separate session with null stdin/stdout and a
private stderr log. Existing services are reused only after identity and audit
health checks. Unknown or stale endpoints are not silently replaced, and failed
startup cleans up only its own child. No supervisor is installed. `status` and
`stop` accept either `--config FILE` for the same resolver or an explicit socket.

The foreground launcher owns its generated settings and binding files. On Agent
exit it calls `release_instance`, cancels and drains that instance, and removes
its own generated files. Other instances and the daemon remain available. A
timed-out release leaves the instance draining and rejects new events; a later
release can finish cleanup without repeating event execution.

`Server` installs no signal handlers; its owner supplies a cancellation flag.
The `aw serve` CLI handles SIGINT/SIGTERM, cancels active work and joins connection
and event workers. A successful `stop` reply acknowledges the request; wait for
the foreground process to exit before treating shutdown as complete. Executor
cleanup and filesystem synchronization are not hard real-time guarantees.

Normal shutdown removes only the socket inode created by that service. It retains
`service.lock` and `journal/`. After a forced kill, an existing socket causes a
visible startup error. Confirm that the previous service process has exited
before explicitly removing that owned `aw.sock`; do not remove a live service's
socket or its lock file. Restarting with the same directory preserves audit
history and allocates a new generation.

## Source entry points

- [User commands and local demonstration](../../../../docs/user-guide/en/user-entrypoint/aw.md#run-the-local-service-demo)
- [Public API](../../crates/aw-service/src/lib.rs),
  [Client](../../crates/aw-service/src/client.rs) and
  [Server](../../crates/aw-service/src/server.rs)
- [Runtime](../../crates/aw-service/src/runtime.rs) and
  [audit metadata](../../crates/aw-service/src/audit.rs)
- [Development checks](../../CONTRIBUTING.md#runtime-validation)
