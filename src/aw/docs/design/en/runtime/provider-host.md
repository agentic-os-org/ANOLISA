# Local Provider Host

[中文版](../../zh/runtime/provider-host.md)

[Design index](../README.md) · [Architecture](../architecture.md)

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
commands or making raw transport understand Provider JSON. Native steps
explicitly select `native-hook/v1alpha1`; the Host uses the same executor to
retain raw bytes and native exit status without a Provider handshake. `aw-host`
does not implement the separate `aw-core::Host` contract,
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

Preparation parses the complete configuration, then calls `admission::preflight`
for enabled requirements whose event/step selector intersection can match the
selected Adapter, before starting any Provider. Provably inactive target steps
are absent from the plan; uncertain normalized/native intersections remain
strictly admitted. Host preparation and native registration consume the same
projection, while revision identity retains the complete document. Each referenced structured Provider then runs
`describe` and `validate_config` in separate processes. Native command Providers
are admitted through the explicit native step contract without those calls.
Boolean hooks (omitted `protocol` or `aw-hook/v1alpha1`) also skip those calls;
local preflight admits their configured match effects without running the script.
Disabled-only references are skipped. The checked replies become evidence for `admission::admit`; only
fully admitted steps are retained. Failure returns no partially prepared Host,
and earlier completed calls are not rolled back. Preparation methods should
therefore avoid side effects.

Successful preparation establishes local protocol agreement. It does not verify
native Hook installation, produce a `Ready` binding or prove that an Agent
adopted a policy. The admitted surface remains:

| Event | Candidate effects | Failure action |
| --- | --- | --- |
| `tool.before` | `observe`, `block` | `report`, or `block` with Adapter support |
| `tool.after` | `observe`, `replace_result` | `report`, or `withhold_result` with explicit Adapter failure-action evidence |

Enabled unsupported events, guards, `ask`, input-replacement
effects and guarantees beyond `native_hook` are rejected. The
[protocol design](../../../../../../docs/developer-guide/en/aw/providers/protocol.md) defines the complete admission contract.

## Adapter execution semantics

For admitted `tool.after` chains with one final replacement, the following table
separates native scheduling from AW's configured step order. Hooks outside the AW
chain retain their native scheduling.

| Framework | Native ordering | Replacement seam | Result seen by AW observers | AW orders inside the service | Completion/correlation seam |
| --- | --- | --- | --- | --- | --- |
| Qoder | Parallel callbacks, or explicitly configured native `sequential` | `PostToolUse` returns `hookSpecificOutput.updatedToolOutput` | Original callback result | Yes: preceding AW attempts finish before the next step; external Hooks are not serialized | Independent callbacks share the same immutable native input and event handle |
| OpenClaw | Result middleware precedes completion; native after handlers run concurrently | `registerAgentToolResultMiddleware` | Sanitized pre-compression result, with native completion metadata | No: the plugin runs AW observers serially at completion, after replacement | Original middleware snapshot binds replacement and observation; the first completion view is pinned |
| QwenPaw | Acting middleware unwinds in reverse registration order | Final `ToolResponse` returned by `on_acting` | Original completed result | No: reverse registration makes AW observers unwind in configured order before replacement | Callbacks share the completed response and tool-call identity; intermediate chunks are not projected |
| AgentScope SDK | Acting middleware unwinds in reverse registration order | Final `ToolResponse` returned by `on_acting` | Original completed result | No: reverse registration makes AW observers unwind in configured order before replacement | Callbacks share the completed response and fresh per-invocation identity; intermediate chunks are not projected |
| Hermes | Completion callbacks precede the final result transform | `transform_tool_result` returns the final string | Original completion result | No: native callback registration preserves the AW observer order | `post_tool_call` and `transform_tool_result` share a binding that normalizes only the seam name and observer-only middleware trace |

Every row shares one event deadline and once-only step claims. A reported failure
does not renew the budget; Qoder releases successors after failed attempts, while
missing callbacks leave an event to expire. The service has explicit Qoder
ordering and OpenClaw/Hermes correlation branches; this is the current supported
adapter contract, not a framework-independent scheduling guarantee. See the
[shared event lifecycle](local-service.md#shared-event-lifetime) for binding and
cancellation details.

The executable contracts are in
[`posttool.rs`](../../../../crates/aw-cli/tests/posttool.rs),
[`openclaw.mjs`](../../../../crates/aw-cli/tests/openclaw.mjs), and the native
[QwenPaw](../../../../crates/aw-cli/tests/fixtures/qwenpaw/native_middleware.py) and
[Hermes](../../../../crates/aw-cli/tests/fixtures/hermes/native_contract.py) fixtures.
The [AgentScope lifecycle acceptance](../../../../crates/aw-cli/tests/agentscope_lifecycle.py)
also exercises the public SDK with a real Tokenless CLI and offline model.
Fixture success alone does not establish real-model adoption.

## Default invoke-only hooks

A boolean hook uses only `invoke`, optionally omitted from its step. The Host
binds and validates an invocation internally, sends only its normalized event
to the command, and requires exit 0, complete stdin delivery and one JSON boolean.
Hook size/depth limits apply to the bare event; the internal envelope does not
consume its quota. Structured Provider requests keep their full-message limits.
True projects all admitted step effects with its optional configured reason code;
false projects none. AW constructs and checks the correlated outcome itself.
No protocol fields or effect response objects are needed in the script. The
Host caps boolean stdout at 32 bytes as well as the configured ceiling and uses
the ordinary executor's immediate group cleanup. Shared deadlines, cancellation,
once-only claims and failure actions remain the existing Host rules. Explicit
structured Providers retain their handshake and response validation; no failed
handshake is treated as an absent method or retried as a boolean hook.

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

Native adapters may call `Event::invoke_with_native_environment(step_id, snapshot)`
to supply the actual callback environment for raw commands. It replaces the bound
environment only for native transport; structured Providers retain the prepared
context. `Event::invoke` remains available and uses the bound environment. The
snapshot is limited to 4096 entries and 1 MiB and rejects invalid keys and NUL.
It changes neither event identity nor deadline, and is not included in stdin or
execution reports. Invalid snapshots consume the step claim and fail visibly.

Use `Event::steps()` to select retained steps, then `Event::invoke(step_id)`.
The iterator retains all admitted steps in configuration order, including steps
whose tool selectors will miss. Within `invoke`, event-level and step-level
`match.tools` intersect after normalization, binding checks and the once-only
claim. Omission or `["*"]` matches all tools at that level. Standard selectors
match normalized `tool.name`; `native:<adapter>:<name>` compares `agent.adapter`
and retained `tool.native_name` exactly. Matching is case-sensitive, without
glob or regex patterns.

An unmatched invocation validates the normalized event and checks cancellation
and the shared deadline before returning `StepOutput::Skipped`. It starts no
Provider or Hook invocation process and emits no candidate effects. Enabled
structured Providers still complete preparation discovery and configuration
validation. Skipped steps keep their claim, correlated record and ordinary
completion path, allowing successor steps and terminal cleanup to proceed.
The service records `kind: skipped` in its completed metadata audit. Invalid,
expired or cancelled events remain failures under the configured `on_error`.

Step order is available to the caller, which owns serial or parallel scheduling
according to its native callback contract. Calls on the same Event share the
deadline even when concurrent. Each step can be claimed once per Event,
including failed attempts; there is no automatic retry. Creating another Event
does not deduplicate a native callback. Before dispatch, the Adapter must collect
the results required by its own contract.

Structured exchanges use one process per method, literal argv, one JSON request
and one JSON response. For native steps, use `Host::hook_event` with the original
callback bytes as well as the normalized event. The raw input is immutable for
that Event; native commands receive it without AW protocol wrapping. Transport
retains its separate one-second cleanup budget; this
can extend return time beyond the event deadline. Unverifiable cleanup remains
an execution failure. Process-group cleanup is not a sandbox or OS enforcement;
see [bounded command execution](bounded-execution.md).

## Interpret reports

`Invocation.result` retains either a `StepOutput` or the original `Failure`.
`StepOutput::Skipped` records a selector miss with no command process or effects;
the Adapter returns a neutral native reply and preserves pending permission
requests and denials. Unsupported input rewrites remain rejected. Tokenless
PreTool suggestions require a fresh call through selected security and native
permission checks. PostTool replacement remains confined to the final authorized
text slot, retaining the native envelope and failure status.
`StepOutput::Provider` contains the checked structured outcome;
`StepOutput::Native` contains stdout, stderr and native process status. A native
nonzero exit is a result for the Adapter to interpret, not a structured Provider
error. The following effect and response-validation rules apply to structured
Provider outcomes.
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

The [Host example](../../../../crates/aw-host/examples/host.rs) reads the
[dedicated configuration](../../../../crates/aw-host/examples/aw.yaml), uses the
absolute current directory as `cwd`, and invokes
`./target/debug/examples/policy`. It supplies synthetic Adapter evidence and
tool events. The before-tool example returns a candidate block for `DeleteFile`;
the after-tool example returns an empty effect list. Neither launches an Agent
or executes a native tool. The sample policy is not sec-core or a security rule
set.

For tests and the workspace gate, see [Contributing to AW](../../../../CONTRIBUTING.md).
Local process tests establish transport and protocol behavior; native acceptance
still requires evidence that the real Agent installed callbacks and adopted the
returned effects.
