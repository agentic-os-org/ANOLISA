# Claude SDK Adapter

[中文版](../../zh/adapters/claude-sdk-adapter.md)

Explicit resident mode is described in [Resident SDK lifecycle](resident-sdk.md).
The task-worker mode below retains its original one-worker lifecycle and failure policy.

The `claude-sdk` Adapter launches one foreground Python worker with Claude Agent
SDK and its bundled Claude Code CLI, verified at SDK 0.2.88 / CLI 2.1.161. The Python script or `-m`
module explicitly uses `aw_claude.create_client(options)` in its public factory.
AW injects the module and private binding through the launch directory and
`PYTHONPATH`; it does not patch global SDK constructors or infer business entrypoints.

## Factory and readiness

The factory copies options and Hook matcher lists, preserving caller ownership.
It adds one AW callback for `PreToolUse` and one each for `PostToolUse` and
`PostToolUseFailure`. AW steps run sequentially within that callback and await
bounded `aw hook` processes. SDK/CLI native scheduling and other matchers remain
independent. The initialization handshake must complete before the factory writes
the same-user readiness receipt. A worker which never creates the client cannot
claim successful AW startup. One worker owns one task binding; Gunicorn child
workers, persistent multi-task servers and remote executors require separate
lifecycle contracts and are not automatically supported by this launcher.

Preflight executes the actual bundled binary's version probe. The factory pins
that binary and rejects caller `cli_path` overrides or missing bundled executables;
there is no PATH fallback. Model configuration and credentials remain SDK-owned.
The worker options cwd must equal the AW launch cwd.

## Submission admission and recovery

Configured `input.submit` routes wrap factory `query` and resident `connect(prompt)`
before calling the SDK. `aw_submission` freezes a supported text message, creates
one UUID turn, and reuses `submission::normalize`. The event's session identity is
the SDK input envelope's `session_id` (or the `query` argument, whose native default
is `default`). It is a submission label; the CLI creates its conversation UUID later.
AW's binding instance and unique turn keep clients and sequential inputs separate.
Tool Hooks continue using the CLI's native session and tool-use IDs.

The input task occupies the client during iterator validation and policy calls.
Resident tool invocation accounting begins only after all submission steps and
`CloseInputEvent` cleanup succeed. Every step shares the same immutable input and
Service deadline. A `finally` closes the input event through the existing CLI
close-event bridge, including unvisited steps after rejection and in-flight steps
after cancellation. Cleanup is shielded and acknowledged before the input-task
marker is released. Admission rejection never creates model/tool ledger entries.

Recoverable, audited Provider faults reuse the existing Claude callback fault
classification. `block` returns input denial and `report` returns neutral output;
risk denial remains terminal for this input under either policy. Scope, identity,
protocol, audit and uncertain cleanup errors retain their failure fences. A failed
initial input after resident initialization keeps the connected client available;
startup failures still close the SDK, relay and scope. Foreground checked clients
also serialize accepted responses and reject reconnects. Unconfigured foreground
clients retain the native SDK class and input modes.

Native `UserPromptSubmit` Hooks run after this factory boundary and retain their
vetoes and added context. AW does not scan their later transformations. See the
[Claude integration guide](../../../../../../docs/developer-guide/en/aw/agents/claude-sdk.md#check-user-text-before-submission)
for exact accepted shapes, limits and application recovery.

## Tool policy and identity

The private `aw hook` transport carries an explicit block reason as a JSON string
on stdout with exit 2; stderr remains local diagnostics. A native command exit 2
retains its defined stderr blocking reason through that transport.

A block returns a native `permissionDecision: deny`; callback transport errors
with before `on_error: block` also return deny. Neutral responses preserve existing
native permissions. Existing hooks may veto execution, including while AW is
running concurrently; an existing `allow` cannot erase an AW deny. Original SDK
pre callbacks receive separate input copies; their `updatedInput` responses are
replaced by explicit deny. A wrapped `can_use_tool` callback retains allow/deny,
but a changed `updated_input` becomes deny. These guards cover Python SDK callbacks.
CLI-loaded settings/project/plugin Hooks run outside these wrappers. They can
rewrite an ordinary Bash command after AW checked its public Hook input; the
rewritten command can execute without another AW check. This Adapter provides
public-Hook coverage and has no final-argument execution gate. Applications needing
that guarantee require a separately verified execution boundary. AW never emits
an approval. When a
`tool.before` route is active, the factory rejects nonempty `PermissionRequest`
matchers before client initialization: their nested `decision.updatedInput`
cannot be checked by AW, and CLI 2.1.161 did not enforce their decisions in the
SDK control-protocol fixture. Use supported `PreToolUse` or `can_use_tool`
callbacks instead. Caller `permission_prompt_tool_name` is also rejected with
`tool.before`; the SDK-owned stdio path for the wrapped `can_use_tool` remains supported.

Events retain native session ID and tool-use ID; before/after pair by those IDs
inside their AW instance. Native subagent attribution remains in `native` input.
Standard Bash/Read/Write/Edit names map to AW bash/file_read/file_write selectors;
other tools use their exact native names. Invalid identities, cwd, callback event
or argument shape fail the configured route.

Both native post events map to `tool.after`. Failure results expose the native
error and interrupt flag without replacing results. Measured with SDK 0.2.88
and CLI 2.1.161, MCP tool failures behave the same on both paths: a tool
result that reports an error (the SDK handler contract's `is_error`, mapped to
the wire-level `isError`) reaches hooks as `PostToolUseFailure` with no
`tool_response` (only `error` and `is_interrupt`), and the model receives the
original error result. MCP projection therefore covers successful results
only; failed results stay unchanged, and an executor/business observation
contract remains the source of truth for business-level success.
Denied tools do not receive a synthetic after event. Cancellation reaps owned hook
process groups; it does not promise cancellation of remote tool side effects.

## Bash optimization

The existing Tokenless Provider supplies `pre_tool` and `post_tool`; no SDK-specific
Provider protocol is needed. A PreTool retry suggestion becomes a native deny with
the complete bounded reason, never `updatedInput` or an approval. The model's new
call traverses all before policies and native permission checks again. An early
before veto or fail-closed error closes its AW event, including unentered steps.

With a `replace_result` step, the bridge runs the AW after chain in configured
order on one original payload. Observers and custom steps precede the single final
replacement Provider and retain the original result without a text projection.
All steps share the service event budget. Reported failures retain the native
result and allow later steps while budget remains. Independent native matchers
retain native scheduling and receive copies; their output-rewrite responses are
rejected when AW replacement is configured.

`PostToolUse` for native `Bash`, with non-image, non-interrupted stdout, keeps its
object projection: the final native `updatedToolOutput` preserves stderr and all
other envelope fields. Native HTTP MCP tools (`mcp__*`) project the model-visible
string: the serialized structuredContent when present, or the single text content
block for plain-text results. Replacement returns that string as
`updatedToolOutput`, and the bundled CLI adopts it in the next model request,
preserving the CLI's structuredContent-first semantics. MCP failures never
reach this path: the CLI delivers error results as `PostToolUseFailure`
without a `tool_response`, so they retain their original error form. Media,
multi-block and unknown shapes have no projection and remain unchanged.
Attributed RTK retry commands reuse the Provider's existing ownership
detection to avoid repeated compression.

## Validation boundary

`tests/test_claude_sdk_native.py` uses the real SDK, bundled CLI and SDK MCP server,
and serves deterministic Anthropic responses on an owned loopback HTTP port.
It checks allow/block, Hook failure, original-hook veto/allow/rewrites, permission
rewrites, rejected `PermissionRequest` matchers, native Bash failure, parallel
calls, two turns, 34 policy/exit-9/SIGTERM fail-closed vetoes followed by an allowed call and durable AW journals.
Optional `--tokenless` cases use a built Protocol v2 CLI to check
Bash model-visible replacement, original observer views, failure/report behavior,
retry rejection, skipped managed MCP error compression and preserved HTTP MCP
error results (`PostToolUseFailure`, uncompressed).
It invokes no model or cloud business service. Runtime files and transcripts stay
under its output directory, including private UDS directories removed after service
shutdown. Overlong socket paths fail with an explicit request for a shorter output path. This does not certify a business deployment or untested SDK versions.
The opt-in requirements fix MCP 1.26.0 because SDK 0.2.88's dependency range can
resolve MCP 2.x, which lacks the `Server.list_tools` method used by this SDK.

The factory preserves `setting_sources` (including native default discovery), inline or
file-based `settings`, local `plugins`, and SDK/HTTP/stdio `mcp_servers`. The SDK
loads the original sources in their native order and discovers project Skills;
AW does not parse, copy or translate these configuration files. Existing callback
composition and runtime restrictions above still apply. `extra_args` continues
to allow only `strict-mcp-config: None`; use the SDK options for settings and MCP.
Model/authentication options are unchanged. Native configuration compatibility
does not expand the Hook effects or certify commands executed inside Skills/plugins.
Only one client can be created in each foreground worker.

## Resident model routing

After the SDK handshake and before accepting a query, the pinned native
`get_settings` control returns the CLI's effective environment. The relay combines
it with the process/options environment, preserving native source precedence for
the upstream endpoint, proxy and TLS configuration. CLI authentication remains
native. This upstream snapshot lasts for the client; changes require a new client.
`apply_flag_settings` overlays only the session's relay endpoint and
nonessential-traffic flag; configuration files and other settings stay intact.
Another native read confirms the effective relay endpoint before connect returns.
These controls and relay startup share a five-second deadline. Unsupported
controls, an unadopted route or a non-Anthropic transport reject the connection
and clean up the SDK, relay and scope. There is no direct model fallback.
The `resident-model-settings` acceptance covers inline/file settings, user,
project/local and default sources, credentials, two turns and concurrent clients.

The explicit resident [MCP handler factory](claude-mcp.md) additionally supports reliable MCP success/error classification and single-text replacement. It uses Adapter-owned correlation and suppresses duplicate AW native callbacks for claimed tools.

`tests/claude_submission.py` combines the pinned SDK/CLI, real AW and sec-core
Provider/backend with a recording local Anthropic endpoint. It covers foreground
and resident clients, resident initial inputs, strings and async text, prompt/PII
risks, Provider exit/timeout/invalid-response faults under block/report, cancellation,
iterator errors, native submit veto, subsequent inputs and released resources.
Provider transport faults are injected by a test wrapper; ordinary scans reach the
real backend. Hosted models and production images require separate acceptance.
