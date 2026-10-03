# QwenPaw App adapter

[中文版](qwenpaw-adapter_zh.md)

AW connects the QwenPaw App to the shared AW service through native AgentScope
middleware. The supported runtime is QwenPaw 2.2.2b4 with AgentScope 2.0.8.
The launcher starts a new App process; it does not attach to an existing server.

## Native boundaries

The plugin registers one `on_acting` middleware per admitted AW step. The native
registry orders factories by priority, preserving registration order at equal
priority. The adapter uses priority 100 and registers after wrappers outside
before wrappers. Before steps therefore run in configuration order, and after
steps run in reverse order. An AW denial is also visible to the outer after
wrappers. Existing middleware retains its native position and behavior.

Independent tool calls retain AgentScope's sequential or concurrent dispatch.
AW does not introduce a global tool lock. Intermediate `ToolChunk` values pass
through unchanged; after steps receive only the completed `ToolResponse`,
including its native success, error, denied or interrupted state.

Each callback includes the native session identifier and tool-call identifier.
`tool_call.input` remains its native JSON string in the command input and is
decoded as JSON for the normalized Provider event. `tool_response` is preserved
as native data. The service uses these identifiers to share one event deadline
across steps and reject repeated or mismatched invocations.

QwenPaw does not define a command-hook return protocol. In this adapter, exit 0
observes and continues; exit 2 before a tool returns a native denied response.
Other nonzero exits, signals, callback startup failures and timeouts are callback
errors. The adapter reports these errors and applies `on_error`: `report` keeps
the tool/result moving, while `block` before a tool returns a denied response.
Host cancellation still propagates. Standard output does not rewrite a tool call
or result. Explicit `action: ask`, `action: approve` or `decision: ask` output
produces an unsupported-approval error under that same failure policy; it never
opens an approval UI. Native permissions continue to run before `on_acting`.

## Installation and readiness

The native working directory, profiles, secrets and existing plugins remain
under QwenPaw's control. AW installs only its owned `plugins/aw-native` directory
for the launch and removes that directory after the owned App exits. A
pre-existing directory with the same name is an error. An explicit native
working directory is required; AW does not initialize a model or copy secrets.

The plugin validates configuration and runtime versions before registering
factories. Factory construction performs no configuration I/O because the native
builder logs factory failures and skips the failed middleware. A native startup
hook writes the private readiness receipt only after the App's plugin loading
phase has completed. The launcher checks the private receipt's launch token,
adapter and hook count, and stops the owned process if readiness is not confirmed
in time. The receipt is same-user installation evidence, not process attestation.

Readiness confirms plugin loading, not an operating-system enforcement boundary.
The native App can publish its core readiness before general plugins finish
loading. Users must wait for AW's plugin-ready message before sending requests.
Reloading the App and multiple worker launches are outside this adapter's scope.

## Coverage limits

The App/API entrypoint is supported. The pinned ACP and TUI entrypoints do not
load this external plugin and are rejected before launch. Native permission
denials and externally executed tools return before `on_acting`; they do not
produce AW before/after callbacks. Exceptions without a completed native tool
response do not produce an invented after event.

The first delivery supports before `observe/block` and after `observe`. It does
not implement approval, input/result rewriting, or an OS-enforced final check.
Provider command execution uses the AW instance's launch directory; QwenPaw
continues to manage each tool's own workspace.
Native command steps receive the actual callback environment, including profile
changes made by QwenPaw. Structured Providers retain the launch-bound environment.

## Source and validation

The adapter follows the pinned official
[QwenPaw plugin API](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/plugins/api.py),
[App startup](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/app/_app.py),
and [middleware assembly](https://github.com/agentscope-ai/QwenPaw/blob/3822ec7173d17cf37c8a02f51d3ed5628079e86e/src/qwenpaw/runtime/builder.py).
Native tests exercise the installed AgentScope chain without a model, including
coexistence, nesting, concurrency, streaming, denial and startup receipts. App/API
acceptance with a deterministic local model verifies actual tool execution,
Provider denial, after observation, existing plugin coexistence and cleanup. It
does not certify a cloud model configuration or the browser console.
