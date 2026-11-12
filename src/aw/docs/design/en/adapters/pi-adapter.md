# Pi SDK adapter

[中文版](../../zh/adapters/pi-adapter.md)

Explicit resident mode is described in [Resident SDK lifecycle](resident-sdk.md).
The task-worker mode below retains its original one-worker lifecycle and failure policy.

The Pi adapter supports the public Coding Agent/Core SDKs
`@earendil-works/pi-coding-agent` and `@earendil-works/pi-agent-core`,
with Node.js >=22.19.0 on Linux. Its entrypoint is `sdk`: one AW-owned foreground
worker imports an application factory. Public factory, input submission and
strict output acceptance covered all 24 published matching SDK pairs from
0.84.2 through 1.5.2 with the package conditions in the Pi integration guide.
Other releases must retain the Agent/AgentSession and tool lifecycle interfaces.
The native Pi CLI is outside this contract.

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

## User submission boundary

Configured `input.submit` requires `runWorker`; a legacy `run` application fails
before its factory executes. The resident wrapper begins one execution, assigns a
monotonic AW turn sequence and reads the native session ID. `submission::normalize` emits
`input.text`, `session_id` and `turn_id` without a synthetic tool call. All input
steps reuse the admitted task routes and shared event budget; the tool invocation
ledger is unchanged. A denied or failed blocking check drains the execution and
allows another prompt. Report faults continue with the original text. Input events
use the Service's sequenced submission ledger. Every check acknowledges input
closure before dispatch or recovery, reclaiming its details without allowing replay.
The 1,024-entry bound applies to active inputs, not lifetime submissions.

The supported boundary is literal interactive text passed to `session.prompt`.
`input`, `context` and `before_provider_request` extension handlers, media,
queued/alternate ingress and expanded slash
commands are rejected before native prompt execution. The guarded `agent.prompt`
checks that Pi's final user projection equals the scanned text. Context and payload
handlers run after this boundary, so their presence is rejected both before and
after the asynchronous scan. `agent.continue`
remains available only inside that authorized execution. Tool results, model
continuations and retries do not become input events. See the
[Pi integration guide](../../../../../../docs/developer-guide/en/aw/agents/pi.md#check-user-submissions)
for supported options and lifecycle calls.

Direct Agent callbacks also run after the prompt guard. The boundary wraps and
fixes `transformContext`, `convertToLlm` and `onPayload`, passing isolated values
and returning isolated results. Message callbacks must preserve the ordered user
content; payload callbacks must preserve the complete provider request. This
covers returned replacements, in-place mutation and retained references. A
violation is retained through native assistant-error conversion and rejects the
outer prompt even when Pi consumes the callback exception. Before invoking the
model stream, AW compares user content with the authorized prompt/history
snapshot again and requires the guarded payload callback.

Abort/dispose fences the scope and cancels the input transport before awaiting
native teardown. The worker joins the pending prompt before releasing the scope;
an unsuccessful cleanup poisons the worker. Scope release reclaims input events
whose callback was interrupted before acknowledgement. Late results cannot cross
the scope/execution checks into model transport. Ordinary policy denial leaves
the native session untouched, allowing the next prompt after drain.

## Tool semantics

The original before callback runs first. Its exceptions and denials retain their
Pi meaning; AW does not grant native permission. AW sees validated arguments
including original callback mutations. AW denial returns `{block:true,reason}`;
Pi skips execution and its after callback. Unknown tools, argument validation
errors, early abort and native before failures also skip after. Tool exceptions
reach after with `isError:true`.

The original after callback runs first. AW observes its field-by-field replacement
using Pi's nullish merge semantics, including image normalization, `isError`,
and `structuredContent`. A content override without structured output drops
the stale structured field; AW text replacement does the same.
Native callback exceptions keep the SDK's error behavior; strict output uses a
generic error message to avoid exposing unchecked callback data. AW before supports
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
No global tool lock is added. Legacy task workers retain at most 1024 event
correlations per instance, including closed before/after events. Resident factories
use model-produced identity, active-state reclamation and configurable concurrent
budgets; see [long sessions](resident-sdk.md). A denial before the final configured step explicitly
closes the event without running remaining steps, including native invalid JSON,
nonzero/signal exits and execution failures under `on_error:block`. A failure to
close is reported and never changes a denial into an allow. A managed
`tool.after` step whose Provider invocation fails under `on_error: report`
receives a neutral reply: the audit already records the fault, the unchecked
original result is kept, and the same session continues with its next prompt.
Under `on_error: withhold_result`, audited Provider execution, timeout and
response faults discard the complete result through the native SDK error path;
the model and subscribers receive `AW post-tool check failed; result withheld by policy`, and
the next prompt can use the same session. Before-tool faults, block faults,
cancellation, identity and cleanup failures keep stopping the task. SDK cancellation or a
local transport limit may terminate the helper before it can acknowledge close;
any already opened event then reaches its original deadline or binding release,
without extending its budget. This differs from an awaited policy denial.

## Strict tool output

Configure `scan_tool_output` with `on_error: withhold_result` to require inspection
before tool data reaches the model or Agent subscribers. This route requires both
Pi packages at the same stable version from 0.84.2 through 1.5.2, inclusive.
All 24 published pairs passed the focused native final-event suite for the
`Agent.processEvents` delivery boundary. Prereleases, mixed package versions
and later releases cannot activate strict output until separately accepted.
AW suppresses `tool_execution_update` for the entire target whenever any strict
after route is configured, including tools outside that route's selectors. The
native final event contains the adopted result; AW does not replay partial chunks.
Targets without a strict after route retain native streaming and report-mode
Tokenless behavior.

Before invoking Providers, AW clones the effective result after native callback
overrides. Strict adoption returns detached fields and restores the full checked
snapshot on Pi's newly allocated final envelope before dispatch. This covers
nullish fallbacks and extra execute-result fields that Pi otherwise retains.
References held by tools or callbacks cannot change the scanned result during
Provider calls, cleanup or final delivery.

PII risk uses the existing validated text replacement and preserves native status
and metadata. If inspection fails or the retained envelope contains sensitive
fields, the SDK error path discards the entire original result and supplies the
fixed placeholder. The private `aw_withheld` reply comes only from an audited
Provider fault; native Hooks cannot forge it. Protocol, identity, cancellation and
cleanup failures retain their managed task failure behavior.

## Callback protocol and verification

AW-generated binding/config files remain private to the foreground launch.
Callbacks invoke `aw hook --adapter pi` with literal arguments, bounded stdin,
stdout, stderr and a timeout. Cancellation kills and waits for the callback's
owned process group. Native command output may be empty or a JSON object with
only boolean `block` and optional string `reason`; blocking is valid for
`input.submit` and `tool.before`. Resident submissions invoke the same dialect
through `aw scope --action invoke` with their private task handle.
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
cargo build --locked -p aw-cli --bin aw
mkdir -p target/pi-sdk-validation
cp crates/aw-cli/tests/fixtures/pi/package*.json target/pi-sdk-validation/
npm ci --prefix target/pi-sdk-validation --ignore-scripts --no-audit --no-fund
python3 crates/aw-cli/tests/fixtures/pi/public-smoke.py \
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
cargo build --locked --manifest-path ../../providers/tokenless/Cargo.toml
cargo build --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
cargo build --locked --manifest-path ../tokenless/Cargo.toml \
  --target-dir target/t -p tokenless-cli
python3 crates/aw-cli/tests/fixtures/pi/tokenless-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation \
  --tokenless-provider ../../providers/tokenless/target/debug/aw-provider-tokenless \
  --tokenless-cli target/t/debug/tokenless \
  --sec-core-provider ../../providers/agent-sec-core/target/debug/aw-provider-sec-core
```

This runs the real SDK, AW, Tokenless Provider and Protocol v2 CLI, whose linked
RTK produces the rewrite. The model stream, bash tool and scanner backend are
deterministic fixtures; the test does not execute a suggested shell command. It checks selected
compression reaching model context, a fresh retry passing through sec-core again,
and RTK-owned output remaining unchanged. Both telemetry sinks are disabled and
all state is local to the supplied test root. The eight bridge authority/envelope
boundary cases also run inside the default Rust Pi gate.

## Submission acceptance

```bash
cargo build --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
python3 crates/aw-cli/tests/fixtures/pi/submission-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation \
  --sec-core-provider ../../providers/agent-sec-core/target/debug/aw-provider-sec-core \
  --output target/pi-submission
```

This runs Pi 1.3.0, AW and the real sec-core Provider against a local HTTP model
and a controlled scanner CLI. It checks original text, risk/PII refusal and recovery,
Provider exit/timeout/invalid output under block/report, input/context/provider-request extension rejection,
alternate ingress, concurrent sessions/prompts, abort/dispose and empty resource
usage. A separate model-endpoint assertion fails if rejected input arrives.
The scanner fixture does not certify sec-core rules or a deployed business image.

```bash
python3 crates/aw-cli/tests/fixtures/pi/strict-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation \
  --sec-core-provider ../../providers/agent-sec-core/target/debug/aw-provider-sec-core \
  --output target/pi-strict-output
```

This uses the real SDK, HTTP model transport and sec-core Provider with a controlled
scanner. It checks split sensitive chunks, native events registered before AW,
model requests, retained metadata, tool and callback errors, scanner failures,
cancellation, next prompts and new tasks.
