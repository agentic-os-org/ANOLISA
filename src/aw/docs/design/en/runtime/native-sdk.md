# Application-owned SDK protocol

[中文版](../../zh/runtime/native-sdk.md)

The `aw-sdk/v1` extension lets installed application callbacks use the existing
Service and Adapter directly. Applications own their native process, install
callbacks and acknowledge Ready. The Service owns policy preparation, Scope
isolation, Provider execution and durable metadata.

## Negotiation and ownership

Read `status` over `aw-service/v1alpha7` and require `sdk_protocol: aw-sdk/v1`
before sending `registration` with `action: register_sdk`. Keep the discovered
identity on every call. Old services omit this extension; clients must report
unsupported protocol without retrying registration through a legacy action.
`register` and `register_native` retain their strict original fields.

`register_sdk` has the ordinary registration fields plus mandatory `sdk_version`
and `availability` (`required` or `native_fallback`). Its owner is the authenticated
application PID/start time. Same-process instances use distinct recovery keys.
A repeated key with changed mode or input is rejected. Query with the original
key after a lost registration response; inspect terminal failure inside a
successful response. The replay limit, owner-loss cleanup and generation checks
remain those of [independent registration](local-service.md#independent-registration).

For `native_fallback`, each enabled event applicable to the registering target must have
`required: false`. Policy updates apply the same check to the candidate revision
and the registered target. A conflicting update leaves the active policy intact. Providers may still explicitly deny,
and `on_error: block` retains its online behavior. Neither a policy error nor a
Provider failure establishes transport unavailability.

## Ready, task Scope and callbacks

Registration returns the canonical Adapter's routes and policy revision. Install
those callbacks before acknowledging Ready. Supported entrypoints remain the
semantic `sdk/application` profile and ordinary resident Qoder SDK, QwenPaw,
AgentScope, Pi and Claude SDK routes.
Capabilities describe installed seams; public registration imposes no exact
Agent/Worker version list. Native framework acceptance remains specific to the
actual integration and version tested.

Each task opens a Scope, reads `scope_policy`, installs/confirms the returned
routes and sends `acknowledge_scope` within the handoff deadline. SDK Scopes
require that policy read. Callbacks before acknowledgement or on the root fail.
All direct operations retain the owner/direct-child peer check. Ready/ACK records
an application acknowledgement of route installation; individual Query wiring
and effect adoption require native acceptance evidence.

`SdkCallback` uses the wire method `sdk_callback` with `instance_id`, `event`,
`step_id`, `native_input` (the native JSON object), and `close` (boolean). Service
selects the Adapter and step error policy from the immutable Scope. Adapter
normalization and response adoption are shared with the existing CLI path.
The result is `{code, stdout, stderr}` with byte arrays; code 2 preserves a native
deny. Provider failures return `sdk_step_failed`, and must not become fallback.
The callback records native metadata through the existing Hook path, without
claiming that the application adopted a returned effect.

Follow the returned native hook ordering. One event shares its original budget
and once-only step ledger across calls. `close: true` drains that correlation;
close the Scope after native task cleanup. An uncertain reply must never cause
a retry of a callback, native tool or Provider step. Release affects only the
application's instance and its Scopes, leaving the shared service running.
SDK control messages and `sdk_callback` with `close: true` share the bounded
control connection reserve. Ordinary SDK callbacks remain subject to the work
connection limit, so saturated Providers cannot consume the cleanup channel.

## Shared semantic ingress

Applications with installed callbacks can select `adapter: sdk`,
`version: aw-hook-source/v1`, `entrypoint: application`. This profile accepts a
subset of `input.submit: [observe, block]`, `tool.before: [observe, block]`, and
`tool.after: [observe]`. Unsupported events, effects, failure actions and protocol
versions fail admission. It has no managed launcher: the application owns startup.

Both language clients expose `task.emit(event, payload)`. They bind the version
and event to an `aw-hook-source/v1` envelope and dispatch the immutable Scope's
ordered steps through `sdk_callback`. The Adapter owns normalization; Service,
Host, Providers, budgets and audit retain their existing path. Payload fields are:

| Event | Required fields |
| --- | --- |
| `input.submit` | `session_id`, `turn_id`, `text` |
| `tool.before` | `session_id`, `call_id`, `name`, object `input` |
| `tool.after` | Before fields plus JSON `result` |

Identities and tool names are nonblank and at most 512 bytes. Unknown envelope
fields are rejected. The Service supplies binding and instance identities.
The source supplies stable native session/call identities and a canonical tool
name; duplicate or uncertain invocations must not be retried as new native work.

`emit` returns `decision: continue | block` and
`coverage: checked | unavailable | not_configured`. `checked` means configured
steps or observation completed; it does not attest native adoption. The caller
must stop before submission/tool execution on `block`. Observation alone grants
no blocking or final-delivery guarantee. Missing policy and transport fallback
remain distinguishable. The application must confirm installed boundaries in
`install(routes)` and verify adoption before declaring control effects.

The shared vocabulary has 16 Hooks. The Python `aw_sdk.sources` and TypeScript
`@anolisa/aw-sdk/sources` modules implement the three boundaries above through
explicit function wrapping or object instrumentation. They use one Admission and
Task per session. Qoder and QwenPaw keep their native mapping for richer effects.
Native Hook sources can use the same ingress once their event semantics and adoption
are verified. The explicit wrappers use the existing execution engine and require
no dowhen dependency. Automatic discovery, per-Hook source selection and ptrace
remain planned.

## Client availability contract

Clients must bound first transport-loss detection. Once a fallback task loses
its connection, its remaining callbacks return the native neutral result
immediately. Ignore late responses and retain already adopted effects. Reconnect
through one bounded background path, rediscover the generation, register and
confirm real Ready; only new tasks use that registration. Cancellation, explicit
denial, configuration/protocol errors and Provider failures remain distinguishable.
The language clients implement this state machine; the Service records the
selected mode and enforces its policy compatibility.

## Cooperative control

`status.sdk_control_protocol` advertises `aw-sdk-control/v1`. The `sdk_control`
operation accepts `poll`, `list`, `request`, `query`, `acknowledge` and `withdraw`.
The application's stable key, authenticated PID/start time and Service generation
identify a discovery record independently of its replaceable registration.
Polling publishes immutable target/mode and a Ready handle, then retrieves the
latest bounded intent. Records are bounded by the Service binding limit; dead
owners and announcements idle for ten seconds are pruned, retaining pending
requests until their deadline.

CLI requests select an exact discovery ID. Same-ID retries must match intent and
timeout; only one pending intent is accepted. The Service journals admission and
confirmation. Attach acknowledgement must carry an actual Ready SDK registration
owned by that application; detach confirmation requires its previous root to be
released. Only the application can acknowledge. No control operation launches an
Agent, installs callbacks, changes availability or transfers ownership.

The language loop fences task admission during idle detach. Attach publishes its
connection only after control acknowledgement; a lost reply is recovered using
the same registration and request. Expired requests cannot publish late Ready.
Uncertain callbacks and tools are never retried. The controller records the last
request result; CLI timeout reports unknown confirmation, so callers inspect
discovery before another request. Withdraw only removes discovery. These rules
extend the existing private Unix socket and peer boundary.

## Verification

Shared negotiation and callback fixtures live in `sdk/tests/fixtures/contract.json`.
From `src/aw`, run:

```bash
cargo test --locked -p aw-service --lib runtime::registration
```

These tests cover protocol and mode conflicts, recovery, owner/Ready gates,
canonical callback execution, denial, Provider errors and replay fencing.
Both language package gates cover cooperative selection, busy detach, late Ready
and recovery; real framework acceptance is documented in the integration guides.

The [TypeScript integration](../../../../../../docs/developer-guide/en/aw/deployment/native-sdk.md) implements this lifecycle and packages canonical Qoder public Hooks. Its offline contract and installation tests run in the default AW gate.

## Deferred after-only task admission

The process-local base belongs to `sdk/typescript/src/admission.mjs` and
`sdk/python/src/aw_sdk/admission.py`, sharing the existing Instance/Task contract.
Sources preinstall native capture and pass the installed events to one Admission
per request. `prepare` waits at input when input.submit is installed, or at the
first tool for tool-only policies. The same immutable Scope spans all later
callbacks and native cleanup. The gate adds no Provider Hook; configured before
and after steps retain their original phases. Optional availability can fall
back at its admitted deadline, while explicit errors and cancellation propagate.

Qoder public Hooks reuse Admission for readiness, cancellation and one Task.
The bridge defaults to explicit `native_fallback` availability and rejects an
applicable enabled required event before installing its listener. Callers select `required`
availability explicitly; Service repeats the policy compatibility check on registration and updates.
QwenPaw's configured native plugin does the same through DeferredScope, extending
it only with resident invocation ledger activation, Coordinator streams, native
finalizers and checked model/event publication. It uses required availability.
`QwenPawPlugin.from_config` installs input capture when enabled; before-only and
after-only policies allow the model to start before AW. The compatibility
`AfterOnlyPlugin` keeps its original after-only restriction. Tool-only text
replies create no remote Scope. Earlier input protection requires reinstalling
the Source before accepting new requests; it cannot cover in-flight model work.

The Python SDK Task also owns the required native Scope watch: renew at the
Service-provided interval through long model/tool waits, latch probe or Service
failure, notify the native owner once, and join the monitor before Scope release.
Cooperative detach drains fallback Tasks before root release, so a concurrent
Task close cannot race a deleted child Scope. TypeScript Admission separates
individual waiter cancellation from the shared operation and internal close.

Configuration loaders reuse AW's canonical offline parser. Service retains
capability admission, Ready, Scope revisions and the common Hook execution path
through Host/executor/Provider. Adapter bindings retain native field mapping and
effect adoption; they do not implement another policy engine.
The CLI can compile the target settings into a deployment Descriptor before
SDK startup. A general Source resolver remains a development target.

`--sdk-pid` is a cooperative application selector. The CLI pins the existing
ProcessIdentity before service startup, ensures AW through launch_service, waits
for exactly one matching SDK announcement and sends the normal control intent.
SDK confirmation and process ownership remain application-held. Backend startup
stays with its external deployment; no second lifecycle or Provider protocol is
introduced.

## Deferred public Qoder Hooks

The configuration-driven bridge preinstalls public native callbacks before Query
creation. With input.submit disabled, model dispatch does not open a Scope. The
first tool-before callback waits for cooperative Ready and creates one immutable
Task with that Query's directory and copied environment. Ordered callbacks reuse
canonical Adapter normalization and effect handling; the after callback consumes
the same Scope. Required admission times out before tool dispatch; optional mode fixes native
fallback immediately when attachment is absent. Input policies instead
require admission at native UserPromptSubmit. Registration rejects added input
routes when the bridge was initialized without that source. Native Query close
and callback cancellation precede Scope release. No runtime patch is installed.
