# Local service and client

[中文版](local-service_zh.md)

`aw-service` exposes prepared Provider execution through a standalone Linux
process and a reusable synchronous Rust client. One service owns one immutable
AW configuration snapshot, its live bindings and events, and durable execution
metadata. It does not launch an Agent, install native Hooks or certify that an
Agent adopted a Provider response.

## Component ownership

| Component | Responsibility |
| --- | --- |
| `aw-service::Server` | Private endpoint, configuration identity, binding/event lifetime, bounded RPC handling and audit storage |
| `aw-service::Client` | Versioned requests with one caller deadline and no automatic retries |
| `aw-host` | Provider discovery, configuration validation, admission and once-only step invocation |
| `aw-exec` | Child-process deadlines, output limits, cancellation and process-group cleanup |
| `aw-core::journal::FileJournal` | Durable reservations and verified metadata record chains |
| Future Adapter | Verify native capabilities, normalize callbacks, retain native scheduling and apply effects |

The service reuses `FileJournal` as storage. It does not run Core plans or produce
Core execution/adoption receipts. Provider-private settings remain in
`spec.providers.<name>.config`; no security-engine rules are embedded in the
service.

## Local protocol

[wire.rs](../../crates/aw-service/src/wire.rs) defines the experimental
`aw-service/v1alpha1` protocol. It is separate from the Provider stdio protocol
`aw-provider/v1alpha1`. Each connection carries one length-framed JSON request
and one response; a frame is limited to 2 MiB.

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
| `open_event` | `instance_id`, normalized `event` | Event ID, instance ID and admitted step IDs |
| `invoke_step` | `event_id`, `instance_id`, `step_id` | Candidate effects or execution failure with call metadata |
| `close_event` | `event_id`, `instance_id` | Cancel unfinished work, join children and acknowledge closure |
| `audit` | Preparation or event `key` | Verified durable envelopes and a `terminal` flag |
| `stop` | None | Acknowledge requested shutdown; process exit completes cleanup |

`bind` invokes real `describe` and `validate_config` exchanges. Capability evidence
comes from the trusted same-user caller, never from Provider claims. Preparation
pins the working directory, explicit environment and configured commands for the
binding. Later requests cannot change them. A preparation failure may return an
`audit_key` without a successful binding; the Rust client exposes it through
`Error::Attempt`.

The current service admits `tool.before` with `observe`/`block` and `tool.after`
with `observe`. It rejects enabled unsupported events, `ask`, result replacement,
final guards and stronger execution guarantees. A successful empty effect list
adds no restriction and grants no native permission. A successful policy block
is distinct from a failed invocation whose `failure_action` is `block`.

## Shared event lifetime

An Adapter opens an event once, then sends individual `invoke_step` requests in
the order and concurrency required by its native framework. Client clones can
invoke different steps concurrently. The service adds no serial/parallel policy
and does not rerun failed steps. Each step may be attempted once in that event.

The Adapter must share the returned handle across callbacks belonging to the
same native event. A native session or tool-call ID is correlation data, not
proof that two callbacks belong to one service event. Opening a new event per
step would reset the budget and is not a valid implementation of the shared
event contract.

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
this CLI still requires `--state-dir`. The configuration's startup preference
does not install a supervisor or implement on-demand startup.

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
