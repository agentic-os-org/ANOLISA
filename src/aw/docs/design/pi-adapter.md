# Pi SDK adapter

[中文版](pi-adapter_zh.md)

The Pi adapter supports the public Coding Agent/Core SDKs
`@earendil-works/pi-coding-agent` and `@earendil-works/pi-agent-core` 0.84.2,
with Node.js >=22.19.0 on Linux. Its entrypoint is `sdk`: one AW-owned foreground
worker imports an application factory. The native Pi CLI is outside this contract.

## Factory and lifecycle

`spec.agents.<target>.argv` is `[node, /absolute/application.mjs, ...arguments]`.
AW verifies installed package metadata and the fixed import entrypoints through
Node's standard `node_modules` resolution paths, without importing the application
or running its factory during preflight. Coding Agent's actual Core dependency
is resolved from the Coding Agent installation, including a nested dependency.
Yarn PnP and alternate package layouts are not supported. Runtime imports use
the same verified paths and require a real `AgentSession` and Core `Agent`.
Metadata and readiness are same-user installation evidence, not attestation.

The application exports `createSession({sdk,args,...options})`, returning the
public `createAgentSession` result, and `run(session,{args,createSession})`.
The worker waits for the application factory, composes the installed callbacks,
writes a protected PID/token receipt, and waits for `run`. Replacement sessions
for new/resume/fork/import must use the supplied `createSession` common factory;
it re-applies composition after each application factory invocation. Configure
business callbacks before returning from `createSession`. While managed, the two
callback properties reject assignment, deletion and redefinition, including inside
`run` and on replacement sessions; catching an assignment error leaves AW installed.
Disposal restores callback values and permits assignment again. Guard descriptors
remain for the Agent's lifetime; rebinding requires a fresh factory result. Direct
SDK factories or separately started processes bypass this managed boundary.
The worker releases installed callbacks and disposes its sessions on exit;
AW releases the binding and removes its generated files. The shared daemon and
durable journal remain until explicitly stopped/removed.

## Tool semantics

The original before callback runs first. Its exceptions and denials retain their
Pi meaning; AW does not grant native permission. AW sees validated arguments
including original callback mutations. AW denial returns `{block:true,reason}`;
Pi skips execution and its after callback. Unknown tools, argument validation
errors, early abort and native before failures also skip after. Tool exceptions
reach after with `isError:true`.

The original after callback runs first. AW observes its field-by-field replacement
using Pi's nullish merge semantics, including image normalization and `isError`.
Native callback exceptions keep the SDK's error behavior. AW before supports
`observe`/`block`, including a Provider's literal retry reason. Pi adds the denied
tool result to the next model context; AW never executes a suggestion or rewrites
arguments. A model retry is a new call through all before checks and native permissions.

AW after supports `observe` and `replace_result`. Steps execute in configuration
order against the same effective native result: observers and custom Hooks precede
one final structured replacement step. Only that protected step can return
`aw_replacement`. The bridge validates that only the single text slot changed and
returns a content override while preserving the original callback's other fields.
No additional model message is inserted. Media, multiple blocks and unsuccessful
results are not compressed by Tokenless. Report-mode failures keep the original
SDK callback result; neither observation nor compression rolls back tool effects.

Each preparation reads the current `sessionManager.getSessionId()`, not the
Agent's provider-cache `sessionId`. Each preparation creates a fresh UUID v4 `invocation_id`, used as the normalized
`tool.call_id` for every before/after step. The model's original `tool_call.id`
remains in `native`; Pi may reuse it in subsequent turns. The captured session,
invocation UUID and argument snapshot remain paired until completion, even if
session identity changes.
The bridge tracks at most 512 in-flight prepared calls and clears entries on
native completion events, including abort paths that skip after.
Pi's sequential preparation and parallel execution schedule is preserved:
after events follow completion order, result messages retain source order.
No global tool lock is added. The service's retained correlation capacity still
limits a long-lived worker: there are at most 32 live events and 1024 retained
event correlations per instance, including closed before/after events. Release/rebind
before reaching that limit. A denial before the final configured step explicitly
closes the event without running remaining steps, including native invalid JSON,
nonzero/signal exits and execution failures under `on_error:block`. A failure to
close is reported and never changes a denial into an allow. SDK cancellation or a
local transport limit may terminate the helper before it can acknowledge close;
any already opened event then reaches its original deadline or binding release,
without extending its budget. This differs from an awaited policy denial.

## Callback protocol and verification

AW-generated binding/config files remain private to the foreground launch.
Callbacks invoke `aw hook --adapter pi` with literal arguments, bounded stdin,
stdout, stderr and a timeout. Cancellation kills and waits for the callback's
owned process group. Native command output may be empty or a JSON object with
only boolean `block` and optional string `reason`; blocking is valid only before.
Approvals, input rewriting and native result rewriting are rejected. Nonzero or
signal exits and invalid output use the configured failure action. A native Hook
cannot forge `aw_replacement`; only an admitted final structured replacement step
can emit it through the protected AW configuration.

The default AW gate exercises synthetic launcher contract fixtures. Separate
published-package acceptance exercises the real public factory, real Core loop
and custom tools with a deterministic stream; it uses no model or cloud service.
Its pinned npm lock records package integrity and transitive dependencies:

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
cargo build --locked -p aw-provider-tokenless
mkdir -p target/pi-sdk-validation
cp crates/aw-service/tests/fixtures/pi/package*.json target/pi-sdk-validation/
npm ci --prefix target/pi-sdk-validation --ignore-scripts --no-audit --no-fund
python3 crates/aw-service/tests/fixtures/pi/public-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation
```

The script records commands/PIDs/deadlines, validates allowed and denied side
effects, both awaited hook points, tool failure, existing callback mutation and
replacement, two sessions and parallel completion identity, failure policies,
verified durable Audit chains, binding release and socket removal. Keep state
paths short enough for Linux Unix sockets. This is SDK acceptance, not native
CLI, business deployment, model or remote-executor acceptance.

## Tokenless acceptance

Use the same pinned SDK directory and build the actual Tokenless CLI and Providers:

```bash
cargo build --locked -p aw-provider-tokenless -p aw-provider-sec-core
cargo build --locked --manifest-path ../tokenless/Cargo.toml \
  --target-dir target/t -p tokenless-cli
python3 crates/aw-service/tests/fixtures/pi/tokenless-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation \
  --tokenless-provider target/debug/aw-provider-tokenless \
  --tokenless-cli target/t/debug/tokenless \
  --sec-core-provider target/debug/aw-provider-sec-core
```

This runs the real SDK, AW, Tokenless Provider and Protocol v2 CLI. The model
stream, bash tool, RTK rewrite command and scanner backend are deterministic
fixtures; the test does not execute a suggested shell command. It checks selected
compression reaching model context, a fresh retry passing through sec-core again,
and RTK-owned output remaining unchanged. Both telemetry sinks are disabled and
all state is local to the supplied test root. The eight bridge authority/envelope
boundary cases also run inside the default Rust Pi gate.
