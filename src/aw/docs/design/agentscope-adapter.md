# AgentScope public factory Adapter

[中文版](agentscope-adapter_zh.md)

The `agentscope` Adapter targets AgentScope **2.0.6** on **Python >=3.11**
([SDK requirement](https://pypi.org/project/agentscope/2.0.6/)) and explicitly composed
public `Agent` factories. It shares AW's local service, binding, bounded `aw hook`
client, foreground launcher, readiness and generated-file cleanup. It does not
identify the SDK as QwenPaw or depend on QwenPaw's plugin loader.

## Public factory contract

`aw run --config FILE --agent TARGET` selects the configured `adapter: agentscope`.
`argv` starts with the application Python interpreter, followed by a `.py` script
or `-m MODULE` and application arguments. Interpreter override switches such as
`-I`, `-E` and `-c` are rejected. The same interpreter must report
`importlib.metadata.version("agentscope") == "2.0.6"` during the bounded preflight.

AW supplies an owned `aw_agentscope.py` import path and `AW_NATIVE_CONFIG` to the
foreground worker. The application explicitly changes its public factory:

```python
from aw_agentscope import AwBridge

bridge = AwBridge()
agent = bridge.create_agent(
    name=name,
    system_prompt=system_prompt,
    model=model,
    toolkit=toolkit,
    state=state,
    middlewares=existing_middlewares,
)
```

The Bridge invokes the real public `Agent` constructor, appending AW middleware
to the existing list. Readiness is written only after that constructor succeeds.
The launcher rejects early exit, missing readiness or a receipt from another PID.
The Bridge does not monkeypatch an SDK or discover arbitrary application agents.
**Every new, resumed or reconstructed Agent factory must use this entry.** A
receipt for the first Agent proves that factory's attachment, not coverage of
other factories or routes. Start serving turns only after factory construction.

Existing trusted argument injection and delegation wrappers must precede the AW
middleware; the before event then receives the final `tool_call.input`. Middleware
placed after AW can still rewrite arguments and lies outside this contract.
No native permission gate is replaced or relaxed.

## Native tool semantics

The bridge has three invariants: accepted policy inputs preserve SDK-decoded
argument values and types; each phase keeps one event budget and cancellation
identity, and finalization never creates a fresh event or renews its budget;
Toolkit keeps execution authority and only the admitted final PostTool step may
replace a completed response. Values outside the wire domain follow `on_error`
rather than being rounded or rewritten. Transport deadlines cover subprocess
creation and communication; they do not preempt SDK parsing, tool execution or
host filesystem operations.

AW uses AgentScope's `on_acting` point, inside native parameter validation and
permission checks and immediately outside `Toolkit.call_tool`.

- `tool.before` awaits each step before toolkit execution. Exit 2 adopts a native
  `ToolResponse(state=DENIED)` and skips the tool. Transport failure follows the
  step's `on_error`; `block` denies and `report` continues with a diagnostic.
  The Bridge rejects unsupported approval directives immediately after decoding
  Hook JSON, including BOM-prefixed UTF-8 and UTF-16/32. The CLI preserves native
  bytes; it does not apply a second JSON parser with different encoding rules.
  A JSON decoder recursion failure follows `on_error`: before `block` denies,
  while `report` and after callbacks preserve the native outcome and later steps.
  A denial before the last step closes the shared before event immediately;
  unentered steps do not consume active event slots until the budget expires.
- After wrappers surround before wrappers, so `tool.after` observes a final
  native response including an AW denial without execution. `ToolChunk` is
  intermediate and does not trigger after. Ordinary toolkit errors and native
  interrupted responses retain their original state and content.
- Before steps follow configuration order. Observe-only after chains retain
  reverse configuration order, matching AgentScope's onion semantics. With one
  final `replace_result` step, reverse registration makes after callbacks run in
  declared order: observers and native user Hooks receive the original response,
  then compression changes the final model-visible text. Reported callback failures
  do not skip compression. All steps share one deadline and once-only claims;
  event budgets remain **1..55000 ms**.
- Generated hook configuration grants replacement authority only to the final
  admitted structured replacement step. Native and observation Hook stdout cannot
  replace the result, so every callback keeps the same correlated response until
  that final step. The Bridge validates this authority as an after-only boolean.
- Hook transports in one phase reuse an invocation-local absolute deadline,
  fixed on the first transport attempt at the event budget plus the existing
  three-second transport allowance. Failures before Service acceptance do not
  renew this wait for later steps. An expired phase follows each step's failure
  policy without spawning another callback; the next invocation and the other
  phase receive their own deadline. Cleanup retains its independent close limit.
  Each subprocess is awaited with owned process group cleanup on timeout or
  cancellation. Every entered before/after phase
  registers invocation-scoped cleanup of its first normalized event/session/
  invocation snapshot. An exit-0 report response does not prove the Service
  accepted or completed a step. Phase finalization
  uses an internal `aw hook --close-event true` callback. `CloseHookEvent` fences
  delayed opens under the existing lease lock, cancels active Provider work and
  waits for registry removal without creating an event or renewing its budget.
  The correlation and worker share cancellation before initialization; the lease
  lock is released during initialization. A close timeout retains the cancellation
  fence while blocked initialization drains. Concurrent opens still share one handle.
  Cleanup has a two-second RPC deadline and a three-second subprocess limit
  including subprocess creation; failures are reported without replacing the
  original interruption. Invocation
  finalization drains its owned cleanup task even if cancellation first arrives
  during a close or is delivered repeatedly; all pending phases are attempted
  before cancellation propagates. Each phase retains its bounded close wait.
  Cancellation is not reclassified as a policy denial. An external cancellation
  which prevents a final response is not promised an after callback.
  A report-mode failure leaves the correlation open for later steps.
  The last before step (or an earlier denial) closes its phase before entering
  the tool body. Invocation finalization closes after phases and any before phase
  interrupted by cancellation. An unclaimed step therefore cannot retain event
  capacity while a long tool runs or until its deadline. Cleanup uses the same bounded
  correlation close and reports failures without replacing the native outcome.
  Normal completed events also receive an idempotent close; this costs at most
  two additional close callbacks per tool invocation. Service auto-completion
  and expiry remain available, but cleanup does not depend on detecting errors
  across the process boundary. No failed step is retried.
- Callback JSON escapes Unicode scalar values. JSON-escaped surrogate pairs
  decoded by the SDK as a single scalar remain supported. Literal surrogate code
  points in Python strings (including adjacent high/low pairs and dictionary
  keys) fail with `native_input_invalid_unicode` before transport: JSON must not
  merge distinct Python code points in policy input. `on_error: report` preserves
  native execution and `block` denies instead of aborting the turn.
  JSON encoding recursion also follows the Hook failure action; cleanup reports
  the same failure without replacing the native outcome.
- Integer values in callback JSON must fit the `i64`/`u64` range
  (`-2^63` through `2^64 - 1`). Larger integers fail with
  `native_integer_out_of_range` before transport and follow the configured Hook
  failure action, preserving exact native values under `report`.
- Diagnostics are best-effort, capped at 4096 bytes per write. Pipe/terminal
  writes use an independently opened nonblocking descriptor without changing
  the application's inherited flags. Full, closed or unsupported sinks drop
  diagnostics; policy denials retain their complete reason. Failure diagnostics
  use the same path instead of invoking application logging handlers.
  Regular-file output retains filesystem write semantics; the transport deadline
  does not bound an externally stalled filesystem. No background logger is owned.
- Each invocation reads `agent.state.session_id` at the callback point and assigns
  a fresh `invocation_id`, shared across all its before/after wrappers and used as
  the normalized `tool.call_id`. The native payload retains `tool_call.id`, which
  may repeat across turns in the same session. Independent tool calls and agents
  retain native concurrency. Session/call IDs are required and limited to 512
  UTF-8 bytes.
- Policy input and PostTool command projection use `effective_input`, decoded
  with the pinned SDK's `_json_loads_with_repair` helper without a schema, just
  as `Toolkit.call_tool` decodes execution arguments. Trusted application
  middleware still runs first. The Bridge keeps the original `tool_call.input`
  string in native data and does not rewrite the SDK call. Decode failures,
  including recursion limits, follow the configured Hook failure action.
  For arguments rewritten by preceding trusted middleware, `block` denies;
  `report` lets the SDK produce its native tool error without aborting the turn.
  Upgrading this private codec requires revalidating the SDK pin; it is not a
  new middleware entrypoint.

Native validation/permission failures before `on_acting`, external execution
requests, direct `Toolkit` calls and application dispatch routes do not enter
this middleware. They need their own integration boundary. Framework terminal
success is separate from business failure encoded in returned content.

## Launch and deployment boundary

This first contract supports one foreground Python worker, not a forking
Gunicorn supervisor, automatic attachment in child workers or remote sandboxes.
A long-lived business service needs explicit per-worker/session launch and
binding ownership. The local service retains at most 1024 event correlations per
instance (roughly 512 before/after pairs) until release; session IDs alone do not
reset that budget. No remote command cancellation, hot reload or OS isolation
is implied.

The existing `native-hook/v1alpha1`, structured Provider and default Boolean Hook
contracts all remain available. A Boolean Hook `true` emits its configured
effects: a before step with `effects: [observe, block]` blocks when true; `false`
means no match. Boolean after Hooks admit observe only. A structured Provider can
replace one final text slot while preserving response ID, state, metadata and
content-block fields. Media, multiple blocks, unsuccessful results and intermediate
chunks are not compressed; invalid replacements report failure and retain the
original. Errors cannot undo an already executed tool.

The [SDK lifecycle configuration](../../crates/aw-service/examples/aw.agentscope-lifecycle.yaml)
uses the existing sec-core `scan_code`/`observe_tool` and Tokenless
`pre_tool`/`post_tool` operations. PreTool denies the original attempt and sends
full retry arguments to the model. A new call must pass native permissions and
sec-core again. Attributed RTK ownership comes from the new executed arguments,
not the rejected call ID, and prevents double compression. Keep the exact Toolkit
tool names and input pointers in the sec-core mapping. RTK must be installed
separately; `aw-package configure` continues to target Qoder/OpenClaw.

## Validation

Standard `aw-service` tests check preflight rejection, callback normalization and
failure to adopt a version-only process without a factory/readiness receipt.
These tests require no SDK installation.

The separate real SDK acceptance test runs a deterministic offline model through
public `Agent.reply`, native ReAct scheduling, Toolkit and the entire service/
`aw run` path. It checks streaming, before wait, deny with no tool side effect,
ordinary error, tool interruption, concurrent sessions, same-session tool ID reuse
with identical/changed arguments, 34 consecutive early denials per session within
a 55-second event budget followed by success, two-instance isolation, instance
cleanup and verified durable journal chains:

Native Hooks additionally alternate exit 1 and SIGTERM over 34 consecutive calls
per session: a report step continues to a block step, which closes the before
event and denies the tool. A final allowed call proves event capacity is available.

```bash
# In a Python environment with agentscope==2.0.6 installed, from src/aw:
cargo build --locked -p aw-service --bin aw
python -B crates/aw-service/tests/agentscope_native.py \
  --aw target/debug/aw --output target/agentscope-native
python -B crates/aw-service/tests/agentscope_cancellation.py \
  --aw target/debug/aw --output target/agentscope-cancellation
python -B crates/aw-service/tests/agentscope_arguments.py \
  --aw target/debug/aw --output target/agentscope-arguments
```

Cancellation acceptance interrupts 34 non-final before callbacks and 34 non-final
after callbacks, then another 34 before and 34 after turns while a middle callback
is still awaiting spawn after an earlier step opened the event. All 136 cancellations
use public SDK turns in one live binding. Every cancelled
turn releases its event and native Hook child before continuing; subsequent
normal turns run every policy step. Another 36 turns inject pre-spawn `EMFILE`
at the first, middle and last before/after callbacks. Report failures preserve
later callbacks, a block failure skips the tool body, and every turn immediately
releases its event. Service tests also cover cancellation before
open, repeated cleanup and isolation by instance, session and event.

Another 36 turns route one real Hook child through a private Unix socket proxy:
`open_hook_event` succeeds, but `invoke_step` is disconnected before reaching the
Service. Report-mode exit 0 preserves later steps and block skips the tool body;
phase finalization releases the partial event immediately. Before faults also
assert zero active events at tool entry. The proxy and its
owned connections are bounded and removed on exit.

Another 48 real native Hook turns cover `ask`/`approve` actions and `decision: ask`
across eight UTF encodings with both failure policies. Report continues subsequent
steps; block yields `DENIED` without executing the tool. Every turn releases its event.

Three more native Hook turns return 10,000 nested arrays (20,001 JSON bytes,
within the fixture's 32 KiB output limit) at before-report, before-block and
after-report steps. They verify decoder failure diagnostics, later-step
continuation, denial without execution, native result preservation and event release.

Argument acceptance compares before/after policy input with SDK permission and
tool execution arguments under both `report` and `block`. It covers a missing
closing brace, single quotes, fenced JSON and nested Unicode data, including a
repaired call denied by AW without executing the tool body. A preceding trusted
middleware also injects 10,000-level arguments after native validation: report
preserves the SDK tool error, block denies without execution, and later turns
succeed under both policies.

All outputs and temporary state stay in the selected target directory. Owned
temporary directories are removed after acceptance. The test rejects an output
path whose Unix socket would exceed Linux's 107-byte pathname limit; use a short
worktree/target path. It requires no cloud account,
credentials or model requests; it is not deployment acceptance for a business
container or a modified SDK release.


The additional lifecycle acceptance uses the same real SDK, public turn/Toolkit
path, real AW Providers and a Protocol v2 Tokenless CLI. Its scanner is a
fixture, and its Toolkit records execution without running shell commands.
It verifies full retry arguments, fresh permission/security checks, RTK ownership,
model-context compression after original-result observers, reported custom/native
Hook failures, and unchanged error, interruption and media results. The middleware
unit test additionally checks streaming identity, response envelope preservation
and malformed-replacement fallback.

```bash
cargo build --locked --workspace
python -B crates/aw-service/tests/fixtures/agentscope/middleware.py
python -B crates/aw-service/tests/agentscope_lifecycle.py \
  --aw target/debug/aw --tokenless /absolute/path/to/tokenless \
  --output target/agentscope-lifecycle
```
