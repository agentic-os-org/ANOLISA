# QwenPaw App adapter

[中文版](../../zh/adapters/qwenpaw-adapter.md)

AW manages QwenPaw App tool invocations through the pinned QwenPaw
`2.2.2.post1` / AgentScope `2.0.10` native middleware and Coordinator. Ordinary
`aw run` admits each Agent request into the shared SDK task scope. Native
permissions, scheduling and model configuration remain owned by QwenPaw.

## Admission and identity

The App startup receipt confirms plugin loading. Before an Agent is returned by
`AgentBuilder.build`, AW verifies its actual reply and acting middleware lists.
The native middleware assembly guard requires exactly one resident middleware
and one Coordinator. A required factory exception, skipped middleware or missing
Coordinator rejects construction even though QwenPaw logs and skips ordinary
plugin factory failures. The resident middleware runs before the Coordinator.

Each request acquires an acknowledged child scope through the same CLI transport
and service invocation ledger used by Claude SDK. Each native dispatch receives
a fresh UUID in `aw_invocation_id`; `tool_call.id` retains QwenPaw's original ID.
The service correlates the UUID, and the adapter retains only active native
ownership records. Arguments are compared exactly at the native execution
boundary, including integers above `2**53`, before the shared wire normalization.
Admission uses the pinned Toolkit parser and tool schema, including native empty
input and repair semantics. The Toolkit parser corroborates the effective mapping
again immediately before the admitted dispatch; a later rewrite fences the owner.
The dispatch guard is consumed before the tool body, allowing nested Toolkit
calls to use their own arguments.
Changed inputs, stale native callback identities and retired scopes fail closed.

A request scope stays alive while any tool is offloaded. Completion retires the
invocation after the native stream, native Hooks and Coordinator cleanup finish.
The final request release waits for its active invocations. Sequential completed
calls release their ledger entries; the historical 512-call accumulation limit
applies only to the legacy reloadable path. Active saturation rejects new work;
a later call can enter after existing work drains.

## Native results and cancellation

Before Hooks run in configuration order. After Hooks run in reverse registration
order, preserving the existing final replacement ordering. An AW denial produces
a native denied `ToolResponse` and runs the admitted after Hooks without entering
the native tool. Resident transport preserves denial text, including Tokenless
retry arguments and native command explanations. Native permissions still execute before acting middleware.

The Coordinator's native producer is wrapped through `_run_tool_with_hooks`,
`_begin_offload`, `_finalize_completed` and `_supervise`. AW joins shielded native
after-Hook tasks, holds terminal responses until generator exhaustion, and keeps
an offload's ownership through background processing and completion callbacks.
Background results stay in a private completion entry until the processor and all
completion handlers succeed. Handlers receive that entry with the original context,
task and stream; public HTTP getters and next-Agent hints see no terminal result
before publication. Offload notification callbacks are part of this barrier.
Failed callbacks publish neither cached output nor hints. Publication commits the
validated result before ledger retirement: a later finish/release failure fences
future worker activity while already-committed output remains readable.
Native shutdown and nested cleanup share one ten-second deadline, including
cancellation drain. Concurrent retirement paths share one finish acknowledgement;
failed acknowledgements fence the worker. Local credentials are released after
confirmed native drain even if the service acknowledgement fails; unfinished native
work retains its lease and blocks new worker admissions.
Public `reply_stream.aclose()` and `agent.close()` join interrupted foreground work
before releasing its scope. Normal request closure preserves offloaded work until
its original callbacks complete. Offload and completion callback exceptions are
sanitized before native logging and mark the owning request as failed.

`on_error: report` reports AW callback transport, size and Provider failures and
preserves the original result. Structured Provider and boolean Hook after steps
may use `on_error: withhold_result`: the prepared ordinary resident adapter declares
that failure action to shared admission, the Host records it on failure, and the
native boundary rejects final delivery and subsequent dispatch. `tool.after block`
is invalid; reloadable and native-command steps cannot request withholding.
Event and step selectors jointly determine which tools withhold streaming chunks.
Every configured callback still reaches the Host so skipped steps advance ordered
execution. Transport failure before the Host confirms a skip retains the admitted
failure action, including withholding; local stream selection does not waive it.
Supported text replacements retain the response envelope, state and structured fields.

Native application Hook, ownership and cleanup exceptions always fence the Agent.
This deliberately strengthens QwenPaw's log-and-continue behavior: an uncertain
credential finalizer or original Hook cannot become a successful result through
an exception-swallowing boundary. Other native Hooks retain their order and normal
results. A cleanup deadline cancels and briefly joins owned work, then retains an
unfinished task until its eventual exception can be consumed; it never waits
without a deadline. A cleanup deadline also fences new requests and dispatches
process-wide, including newly constructed Agents; restart the worker after
correcting the cause. Other native Hook failures fence their owning Agent.

## Task-private execution extension

The versioned lease fields and explicit consumer requirements are defined in
[Private execution leases](private-execution.md).

An application may supply `ctx.aw_execution_factory`, an async context manager
that yields `PrivateExecution`, and declares `ctx.aw_environment_version = 1`.
Its native tool subprocess boundary consumes `execution_environment()` explicitly
as `env=`. The context-local environment follows Coordinator-created tasks;
AW never writes task credentials to `os.environ`. Missing declared native
support rejects admission before opening a scope.

The lease provides a separate Hook environment and a result redactor. Result
redaction precedes native after Hooks, external AW Hooks and supported output
processing. Private leases withhold intermediate chunks from both public replies
and native stream subscribers, including report-mode requests. Redaction sees
the accumulated terminal response so a secret split across chunks stays private.
The redactor must be idempotent: applying it again to already-redacted content
must preserve that content. Multiple owned delivery and Hook boundaries apply it.
The redactor must preserve authorization-bearing tool arguments;
changing them rejects the call. Put credentials in the private environment.
Lease cleanup runs after task drain on success, denial, failure or cancellation.
AW clears its environment and secret dictionaries. Credential-file creation,
process-owner-loss cleanup and application deployment integration belong to the
application's lease implementation and require its separate acceptance.

## Entrypoints and compatibility

The launcher owns only `plugins/aw-native`, installs its private runtime assets,
and removes that directory after the owned App exits. Existing plugin directories
and native profiles remain under their owners. Use an initialized absolute
`QWENPAW_WORKING_DIR` and the App/API entrypoint. ACP, TUI and multiple workers
remain outside this adapter's admission contract.

`--reloadable` retains the previous whole-request/tool busy tracking and policy
update behavior. Its binding cannot open resident child scopes; attempting to
combine both modes fails explicitly. Native evidence capture is rejected for an
ordinary resident launch because the shared child-scope observation contract is
unavailable. Reloadable evidence behavior is unchanged.

Native permission denials and externally executed tools complete before acting
middleware and therefore produce no AW tool callbacks. A zero-step ordinary
configuration still installs the resident admission and cleanup middleware.

## Validation

The every-PR `qwenpaw` Rust target loads the actual staged plugin through
`AwPlugin.register`, its factory and installation guard, then exercises `aw run`,
configuration admission, Scope CLI, service and Provider. Its native framework
doubles isolate launcher wiring: withholding failure, report continuation,
before denial, registration failure, cleanup and reloadable rejection. Actual
pinned-framework lifecycle semantics are covered separately below.

`native_resident.py` exercises the installed AgentScope acting chain, QwenPaw
Coordinator and actual middleware assembly. Its transport double isolates native
ordering, strict Post failure, cancellation, offload completion, input integrity,
private environment/redaction, saturation recovery and 1,025 sequential calls.
`native_resident_service.py --bin <directory>` exercises the same Coordinator
through the real Scope CLI, service and Provider for denial and completed drain.
The native inventory retains the earlier middleware, reload and App suites.

These deterministic tests use no model credentials. Full App deployment,
application credential lifecycle, real security backend composition and production
traffic acceptance remain separate checks on the complete SDK/MCP integration.

## Source references

The original plugin integration was based on pinned QwenPaw commit `3822ec7`:
[plugin API](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/plugins/api.py),
[App startup](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/app/_app.py),
[AgentBuilder](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/runtime/builder.py).
Current resident acceptance uses the installed `qwenpaw==2.2.2.post1` and
`agentscope==2.0.10` distributions declared in `tests/native-matrix.json`, including
`qwenpaw.tool_calls`, `qwenpaw.agents.react_agent` and
`agentscope.tool._toolkit`. Revalidate these native seams when changing those pins.
