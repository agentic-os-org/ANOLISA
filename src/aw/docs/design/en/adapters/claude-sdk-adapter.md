# Claude SDK Adapter

[中文版](../../zh/adapters/claude-sdk-adapter.md)

The `claude-sdk` Adapter launches one foreground Python worker with Claude Agent
SDK 0.2.88 and its bundled Claude Code CLI 2.1.161. The Python script or `-m`
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

## Tool policy and identity

The private `aw hook` transport carries an explicit block reason as a JSON string
on stdout with exit 2; stderr remains local diagnostics. A native command exit 2
retains its defined stderr blocking reason through that transport.

A block returns a native `permissionDecision: deny`; callback transport errors
with before `on_error: block` also return deny. Neutral responses preserve existing
native permissions. Existing hooks may veto execution, including while AW is
running concurrently; an existing `allow` cannot erase an AW deny. To prevent
unchecked final arguments, original pre callbacks receive separate input copies;
any `updatedInput` response is replaced by an explicit deny. A `can_use_tool`
callback retains allow/deny, but a changed `updated_input` becomes deny. Argument rewriting and
approval are outside this contract. When a
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
error and interrupt flag without replacing results. CLI 2.1.161 emits
`PostToolUse` even for SDK MCP responses with `isError: true`, and its callback
payload omits that flag. A post event is therefore not proof of business success;
MCP error classification needs an executor/business observation contract.
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

Only `PostToolUse` for native `Bash`, with non-image, non-interrupted stdout, has a
replacement projection. The final native `updatedToolOutput` preserves stderr and
all other envelope fields. Failed tools, MCP tools, media and unknown shapes have
no projection and remain unchanged. MCP result replacement needs a trusted business
status contract because this CLI omits `isError`. Attributed RTK retry commands
reuse the Provider's existing ownership detection to avoid repeated compression.

## Validation boundary

`tests/test_claude_sdk_native.py` uses the real SDK, bundled CLI and SDK MCP server,
and serves deterministic Anthropic responses on an owned loopback HTTP port.
It checks allow/block, Hook failure, original-hook veto/allow/rewrites, permission
rewrites, rejected `PermissionRequest` matchers, native Bash failure, parallel
calls, two turns, 34 policy/exit-9/SIGTERM fail-closed vetoes followed by an allowed call and durable AW journals.
Optional `--tokenless` cases use a built Protocol v2 CLI and RTK on PATH to check
Bash model-visible replacement, original observer views, failure/report behavior,
retry rejection and skipped MCP error compression.
It invokes no model or cloud business service. Runtime files and transcripts stay
under its output directory, including private UDS directories removed after service
shutdown. Overlong socket paths fail with an explicit request for a shorter output path. This does not certify a business deployment or untested SDK versions.
The opt-in requirements fix MCP 1.26.0 because SDK 0.2.88's dependency range can
resolve MCP 2.x, which lacks the `Server.list_tools` method used by this SDK.

The options factory requires `setting_sources=[]`, no `settings` override and no
CLI plugins. `extra_args` allows only `strict-mcp-config: None`; the SDK native
`strict_mcp_config` option is also preserved. Model/authentication options are
unchanged. Applications which discover project skills through settings sources
need an explicit integration design before using this restricted first bridge.
Only one client can be created in each foreground worker.
