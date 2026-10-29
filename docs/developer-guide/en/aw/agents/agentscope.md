# AgentScope SDK

[中文版](../../../zh/aw/agents/agentscope.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start AgentScope SDK

Use a Python environment with `agentscope==2.0.6`; the SDK is separate from
QwenPaw's verified AgentScope 2.0.10 runtime. `argv` selects the SDK environment's
Python and a `.py` application or `-m MODULE`. AW verifies that exact SDK version,
supplies `aw_agentscope` and a protected runtime configuration, and awaits a
receipt after the real public factory has installed middleware.

From `src/aw`, with that environment's `python3` on PATH:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.agentscope.yaml --agent agentscope
target/debug/aw status --config crates/aw-service/examples/aw.agentscope.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.agentscope.yaml
```

The example uses a deterministic offline model and a local Echo tool. It needs no
credentials. Its before Boolean Hook returns false (no block match) and after
returns true (observe). Change the before command to `printf true` to deny the
tool; the native final response becomes denied and after observes that denial.
Stop the service before editing its configuration.

For an existing application, use `AwBridge().create_agent(...)` in **every** public
factory, passing the existing middleware list unchanged. Existing trusted
argument injection must precede the appended AW middleware. A readiness receipt
covers the constructed Agent, not other factories or direct dispatch routes.
The [factory contract](../../../../../src/aw/docs/design/en/adapters/agentscope-adapter.md) details
ordering, budgets, invocation identity and coverage.

Before waits and can block before a tool side effect. After awaits one final
`ToolResponse`, preserving success, error or interruption without repeating
stream chunks. Native parameter/permission rejections before `on_acting`, direct
Toolkit calls and external execution requests need separate integration. The
launcher supports one foreground worker; forking Gunicorn supervisors and
multi-worker/remote deployment are outside this contract. Long-lived processes
must manage the service's 1024 event-correlation limit per instance explicitly.

For PreTool retry suggestions and final PostTool compression, copy the
[SDK lifecycle example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml),
replace all paths and map your Toolkit's exact tool names/argument pointers in
sec-core. It runs security checks before Tokenless PreTool, observes the original
completed response, then compresses one successful text slot. Keep custom/native
after Hooks before the final compression step; with replacement enabled AW runs
this chain in declared order. Native and observation Hooks cannot replace the
response; only the final admitted structured replacement step can change its
model-visible text. Reported Hook failures retain the original input for subsequent
steps. Observe-only chains keep native reverse unwind order.

PreTool denies the original call and returns full retry arguments to the model;
each new attempt passes native permissions and sec-core again. An attributed RTK
retry avoids double compression even with a new call ID. Install RTK separately
for PreTool; PostTool works without it. See [Tokenless behavior](../../../../user-guide/en/aw/tokenless.md)
for text-slot eligibility, failure handling and telemetry configuration. Invalid
final-response replacement directives report failure and retain the original result.
