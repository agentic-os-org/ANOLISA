# Local Provider Host

[中文版](provider-host_zh.md)

`aw-host` connects validated configuration and the `aw-provider/v1alpha1`
protocol to Linux command execution. It executes real Provider processes and
returns checked candidate effects and correlated call reports. Agent startup,
native Hook installation, effect adoption and persistent audit remain with the
embedding application.

## Composition boundary

| Layer | Responsibility |
| --- | --- |
| `aw-config` | Parse desired configuration and validate static references |
| `aw-provider` | Check protocol messages, offline preflight and capability admission |
| `aw-exec` | Execute literal commands with bounded byte transport and owned cleanup |
| `aw-host` | Retain execution context, prepare Providers and invoke admitted event steps |
| Embedding Adapter | Supply trusted capabilities, normalize events, schedule steps and apply native effects |

The Host reuses these libraries without making offline admission execute
commands or making raw transport understand Provider JSON. Native hook command
callers continue to use `aw-exec` directly, retaining raw bytes and native exit
status. `aw-host` does not implement the separate `aw-core::Host` contract,
create Core receipts or change Core's final-dispatch requirements.

## Prepare a fixed context

Call `Host::prepare(bytes, target, capabilities, context, deadline, cancelled)`.
`bytes` is the complete configuration document; its exact bytes determine the
SHA-256 revision retained in every call. `target` selects a configured Agent.
The caller supplies `AdapterCapabilities` for the actual version and entrypoint;
Provider declarations cannot supply or expand that trusted evidence.

`ProcessContext` supplies an absolute `cwd`, the complete child `environment`
and a separate `stderr_bytes` ceiling. No parent environment is inherited
implicitly. Include `PATH` explicitly when executable lookup needs it. The Host
retains commands, private configuration, Adapter evidence and process context
unchanged. Construct a new Host when any of them changes. This pins values, not
executable files or dependencies: their trust and stability remain the caller's
responsibility.

Preparation parses configuration and calls `admission::preflight` for all enabled
requirements before starting any Provider. Each referenced Provider then runs
`describe` and `validate_config` in separate processes. Disabled-only references
are skipped. The checked replies become evidence for `admission::admit`; only
fully admitted steps are retained. Failure returns no partially prepared Host,
and earlier completed calls are not rolled back. Preparation methods should
therefore avoid side effects.

Successful preparation establishes local protocol agreement. It does not verify
native Hook installation, produce a `Ready` binding or prove that an Agent
adopted a policy. The admitted surface remains:

| Event | Candidate effects | Failure action |
| --- | --- | --- |
| `tool.before` | `observe`, `block` | `report`, or `block` with Adapter support |
| `tool.after` | `observe` | `report` |

Enabled unsupported events, exact tool selectors, guards, `ask`, replacement
effects and guarantees beyond `native_hook` are rejected. The
[protocol design](provider-protocol.md) defines the complete admission contract.

## Invoke one event

Create `Host::event(value, original_deadline, cancelled)` using a normalized JSON
event. Its Adapter and binding must match the prepared Host. Native instance,
session and tool-call identifiers remain event data, not authenticated identity.
The invocation schema is checked before a step process starts.

The returned `Event` keeps one deadline: the earlier of the original callback
deadline and the configured event budget measured from `Host::event` entry.
Pass the original deadline so normalization or other work before that call
consumes the callback's budget. Each exchange is additionally capped by the
Provider timeout. Encoding, execution and response validation count against the
deadline; late or cancelled results cannot become successful policy outcomes.
Preparation similarly shares its caller's deadline across discovery calls.
The protocol's integer `budget_ms` rounds positive remaining time up to the next
millisecond, including sub-millisecond remainders. The exact `Instant` still
enforces the deadline; rounding does not extend it.

Use `Event::steps()` to select retained steps, then `Event::invoke(step_id)`.
Step order is available to the caller, which owns serial or parallel scheduling
according to its native callback contract. Calls on the same Event share the
deadline even when concurrent. Each step can be claimed once per Event,
including failed attempts; there is no automatic retry. Creating another Event
does not deduplicate a native callback. Before dispatch, the Adapter must collect
the results required by its own contract.

All exchanges use one process per method, literal argv, one JSON request and one
JSON response. Transport retains its separate one-second cleanup budget; this
can extend return time beyond the event deadline. Unverifiable cleanup remains
an execution failure. Process-group cleanup is not a sandbox or OS enforcement;
see [bounded command execution](bounded-execution.md).

## Interpret reports

`Invocation.result` retains either an `Outcome` or the original `Failure`.
A successful `block` effect is a policy outcome. A nonzero exit, incomplete
stdin write, transport failure, Provider error or invalid protocol response is
an execution failure. `failure_action` separately reports the configured
`Report` or `Block` action for failures; it never replaces the failed result with
a successful policy block. An empty effect list adds no restriction and grants
no native permission. The Adapter owns the actual tool decision and adoption.

`CallRecord` correlates configuration revision, binding, Provider, request,
method, event and step, with elapsed time and native process facts when
available. `Host::preparation()` exposes successful preparation calls;
`Error::Preparation(Box<PreparationFailure>)` retains that successful prefix in
`completed` when preparation later fails, together with its original `cause`.
An `Error::Call` cause carries the failed call; final admission, deadline or
cancellation failures also preserve the preceding call history. Early configuration,
preflight and context errors remain direct errors with no processes started.
Invocation failures remain in the returned report, while invalid or repeated
step selection returns `Error`. Reports are local data, not persisted audit
records or Core receipts. Raw stderr is retained separately and never
automatically logged; an audit writer must redact or omit diagnostics and
sensitive payloads.

## Local example

Run from the repository root on Linux:

```bash
cd src/aw
cargo build --locked -p aw-provider --example policy
cargo run --locked -p aw-host --example host -- crates/aw-host/examples/aw.yaml tool.before DeleteFile
cargo run --locked -p aw-host --example host -- crates/aw-host/examples/aw.yaml tool.after ReadFile
```

The [Host example](../../crates/aw-host/examples/host.rs) reads the
[dedicated configuration](../../crates/aw-host/examples/aw.yaml), uses the
absolute current directory as `cwd`, and invokes
`./target/debug/examples/policy`. It supplies synthetic Adapter evidence and
tool events. The before-tool example returns a candidate block for `DeleteFile`;
the after-tool example returns an empty effect list. Neither launches an Agent
or executes a native tool. The sample policy is not sec-core or a security rule
set.

For tests and the workspace gate, see [Contributing to AW](../../CONTRIBUTING.md).
Local process tests establish transport and protocol behavior; native acceptance
still requires evidence that the real Agent installed callbacks and adopted the
returned effects.
