# Claude SDK

[中文版](../../../zh/aw/agents/claude-sdk.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start a Claude SDK worker

The verified baseline is Claude Agent SDK 0.2.88 with bundled CLI 2.1.161.
Other releases are admitted through the same native client, Hook, identity and
transport lifecycle checks. The Adapter records the installed SDK version and launches one
foreground Python script or `-m` module and requires the worker's public factory
to use `aw_claude.create_client(options)`. Model credentials remain SDK settings.
AW supplies this module on `PYTHONPATH` for the launched worker; it does not
inject hooks into an arbitrary SDK application automatically.

For the source example, prepare the selected Python environment and run from
`src/aw`. The MCP pin addresses a reproduced incompatibility with MCP 2.x:

```bash
python3 -m venv target/claude-sdk-venv
target/claude-sdk-venv/bin/pip install -r tests/claude-sdk-requirements.txt
```

Copy `crates/aw-service/examples/aw.claude-sdk.yaml` to your configuration and set
its Agent argv interpreter to the absolute `target/claude-sdk-venv/bin/python`
path. Keep the foreground example worker path or replace it with your script.
Its options factory should use this pattern after constructing its normal MCP
tools, permissions, resume settings and existing hooks:

```python
from aw_claude import create_client

async with create_client(options) as client:
    await client.query(prompt)
    async for message in client.receive_response():
        handle_message(message)
```

```bash
target/debug/aw run --config /absolute/aw.claude-sdk.yaml --agent claude -- 'Use echo to return hello.'
target/debug/aw service status --config /absolute/aw.claude-sdk.yaml
target/debug/aw service stop --config /absolute/aw.claude-sdk.yaml
```

Claude SDK event budgets must be 1..55,000 ms. Each `input.submit`, `tool.before` or `tool.after`
route uses `events.<name>.budget_ms` when set, otherwise
`execution.default_event_budget_ms`. `aw run` enforces this adapter-specific
limit after static configuration validation.

The worker's options cwd must equal the launch cwd. Readiness is published after
the real SDK initialization handshake accepts callbacks. AW observes or denies
`PreToolUse`; `PostToolUse` and `PostToolUseFailure` both supply after observations.
Denied tools do not produce a synthetic after. Existing hook vetoes and native
permissions remain effective. Existing Python SDK pre-hook `updatedInput` and
wrapped permission `updated_input` changes are denied. CLI-loaded settings and
plugin Hooks run independently: they can rewrite ordinary tool arguments after
AW's public `PreToolUse` check, and those final arguments are not checked again
before execution. Before policies cover the input observed at that Hook only;
applications requiring final-argument enforcement need another verified boundary.
When `tool.before` is active, nonempty
`PermissionRequest` matchers are rejected before initialization; use supported
`PreToolUse` or `can_use_tool` callbacks instead. Caller-specified
`permission_prompt_tool_name` is also rejected with `tool.before`; SDK-owned stdio
permissions for the wrapped `can_use_tool` remain supported. Caller `cli_path` overrides and missing
bundled binaries are rejected. The worker must not detach or start multiple
SDK clients/tasks within one launcher binding; Gunicorn and business deployments
need their own task lifecycle integration.

Measured with SDK 0.2.88 and CLI 2.1.161, MCP tool failures (the SDK handler
contract's `is_error`, mapped to the wire-level `isError`) reach hooks as
`PostToolUseFailure`, like native Bash execution failures; after observation
still does not classify every business failure.
See the [Adapter contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/en/adapters/claude-sdk-adapter.md)
for identity, cancellation and validation limits.

The factory preserves `setting_sources` (including native default discovery), inline or
file-based `settings`, local `plugins`, and SDK/HTTP/stdio `mcp_servers`. The SDK
loads the original sources in their native order and discovers project Skills;
AW does not parse, copy or translate these configuration files. Existing callback
composition and runtime restrictions above still apply. `extra_args` continues
to allow only `strict-mcp-config: None`; use the SDK options for settings and MCP.
Model/authentication options are unchanged. Native configuration compatibility
does not expand the Hook effects or certify commands executed inside Skills/plugins.
Resident clients resolve the CLI's effective settings before accepting queries.
Their Anthropic Messages relay uses the merged model endpoint, proxy and TLS
environment; native credentials stay with the CLI. A session-only settings overlay
routes model responses through AW without changing configuration files. Connect
fails if native settings controls or relay-route confirmation fail. This requires
the verified SDK 0.2.88 / bundled CLI 2.1.161 settings-control interface.
The relay snapshots its upstream environment at connect; create a new client to
apply endpoint, proxy or TLS changes.
The current sec-core Provider exposes submission, tool PII and Skill operations.
Provider operations and Adapter capabilities are admitted separately: `scan_code`
and `scan_tool_input` check the Claude callback input. `input.submit` supports the
text factory paths below; required output `on_error: withhold_result` remains
unsupported and is rejected before the worker starts.
Native Skill discovery remains available; sec-core `check_skill` resolution is
currently verified for Qoder SDK and QwenPaw, with no Claude resolution contract.
See the [Provider guide](../providers/sec-core.md) before sharing its configuration.

The direct task-worker mode permits one client per worker. Use the [resident entrypoint](../deployment/resident.md) for multiple task clients.

The `claude-sdk` adapter supports this restricted foreground worker factory;
`aw connect` does not provide persistent installation for it, and all `--native-*`
overrides are unsupported.

### Final tool-result security boundary

`tool.after` observes the native Post result. An application callback can still
return `updatedToolOutput` after AW finishes. A required `security.violation`
policy adds a final check inside the bundled CLI, immediately before its HTTP
runtime sends the serialized Anthropic Messages request.

The conditional profile covers Python SDK 0.2.88 / CLI 2.1.161 and SDK 0.2.182 /
CLI 2.1.325, plus TypeScript SDK 0.3.325 / CLI 2.1.325 on Linux arm64 and
x86_64, with exact SDK and executable fingerprints.
Use a fresh foreground `create_client` with `setting_sources=[]`, no settings,
plugins, resume, custom agents or runtime/provider overrides, and a direct
Anthropic-compatible HTTP(S) base URL with an explicit model ID and no fallback.
Gateway path prefixes such as `/apps/anthropic` are supported; model aliases
(`auto`, `default`, `sonnet`, `opus`, `haiku`) are rejected.
Requests with bodies on other routes are rejected, including token refresh and
other model APIs. Set `tools=["Bash"]` or `tools=[]` and use only
in-process SDK MCP servers. Background execution is disabled. Every final step requires `on_error: block`; structured
Provider and Boolean policy steps are supported. Unknown builds fail required
admission; unsupported factory options fail before a query is dispatched.

The private Bun preload covers both global `fetch` and pooled
`FetchSession.fetch`. It binds final text to the native session, call, tool name
and input, invokes the existing AW Hook path, and sends an immutable checked
body. Denial, policy errors, timeouts, missing identity and changed historical
results withhold the tool content. Unsupported request envelopes stop the
request. Tools retain side effects that already occurred. Ordinary Post-only
launches have no final preload.

The native acceptance runs safe and late-mutation cases for Bash and SDK MCP,
plus policy timeout/error cases, and records both HTTP requests and deterministic
local model replies. Opt-in Python SDK 0.2.182 acceptance also exercised Alibaba
Cloud Token Plan with `qwen3.8-flash`: a capture proxy forwarded the exact
inspected bytes, and the hosted model echoed the late marker only with the gate
disabled. This verifies the Claude SDK transport using a Qwen model. Resident
sessions, subagents, alternative model APIs and multimodal delivery are not certified.
Application Hook context injection is rejected in this profile; other observers
that receive results before the model boundary remain outside its scope.

### TypeScript foreground Query

Install `@anthropic-ai/claude-agent-sdk@0.3.325` alongside the application and use
Node.js 22+ on Linux. Configure the same `claude-sdk` adapter with
`argv: ["/absolute/path/to/node", "/absolute/path/to/worker.mjs"]`.
The public factory composes native tool callbacks and waits for SDK initialization
and runtime readiness before releasing the prompt:

```javascript
const { createQuery } = await import(process.env.AW_CLAUDE_SDK_BRIDGE);
const query = await createQuery({
  prompt: "Run printf hello once and report its result.",
  options: {
    model: "claude-sonnet-4-6", settingSources: [],
    tools: ["Bash"], allowedTools: ["Bash"], maxTurns: 2,
  },
});
try {
  for await (const message of query) console.log(JSON.stringify(message));
} finally {
  query.close();
}
```

This factory supports one foreground Query, `tool.before` observe/block,
`tool.after` observe, and the conditional final check. It accepts `cwd`, `env`,
`mcpServers` containing public SDK servers, and native tool `hooks`; application
callbacks run after the AW callback. The returned object exposes iteration and
`close()`. Input replacement, result compression, resident reload, native command
steps and runtime reconfiguration are unavailable in this TypeScript profile.
Unknown TypeScript SDK fingerprints are rejected even for Post-only launches.

### Enable Bash PreTool and PostTool optimization

Use the [shared Tokenless configuration](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml),
select its `claude` target, and replace the absolute executable, worker and state
paths. The source `claude-worker.py` example explicitly exposes and allows native
Bash in addition to its echo tool. Keep your application's own permission policy
when composing its factory. See the [Tokenless guide](../../../../user-guide/en/aw/tokenless.md) for Provider
installation and statistics settings.

PreTool blocks the original Bash attempt and returns retry arguments to the model;
the new call passes all policies and native permissions again. PostTool replaces
only completed, non-image, non-interrupted Bash stdout. Put completion observers
and custom steps before the single final `replace_result` step. They see the
original result; stderr and other native fields are retained. Failure/report keeps
the original result. Native HTTP MCP tools project the model-visible string
(the serialized structuredContent, or the single text block for plain-text
results) and a replacement returns that string for the next model request;
multi-block and media results stay unchanged. The native callback does not
expose reliable business-error status, so MCP error classification still needs
the [managed MCP factory](../deployment/resident.md).

Run pinned native tests through [contributing](../../../../../src/aw/CONTRIBUTING.md).
See [resident deployment](../deployment/resident.md) for resident clients and managed MCP.

Final history receipts retain only digests and a withholding flag, without copying tool text. Early denial, failure or cancellation closes the AW event through a separate bounded cleanup call before returning.

## Native startup and idle policy updates

A native Python host can prepare AW before creating managed SDK clients.
Its `aw.yaml` target `claude` selects the Python environment and worker script.
Install this startup code once and restart the host. The application module
uses the existing `aw_claude.create_client` factory and closes every client.
Start the dedicated host with `setsid python native_host.py`: AW can terminate
that host's process group if SDK cancellation cannot be acknowledged.

```python
import asyncio
import json
import os
import subprocess
import sys

prepared = json.loads(subprocess.check_output(
    ["aw", "instance", "prepare", "--agent", "claude"], text=True))
os.environ.update(prepared["environment"])
sys.path[:0] = prepared["environment"]["PYTHONPATH"].split(os.pathsep)

from aw_resident import prepare_native

async def main():
    await prepare_native()
    from application import run_worker
    await run_worker()

try:
    asyncio.run(main())
finally:
    subprocess.run(["aw", "instance", "release", "--binding", prepared["binding"]],
                   check=True)
```

After readiness, `aw update --agent claude` loads the same default
`aw.yaml`. An open client keeps its policy for its entire conversation;
updates require every client to disconnect and complete cleanup. The next
client fetches and installs the current routes before SDK connection, including
native callback composition and managed MCP checks. Preparation and teardown
hold the same admission reservation, so updates cannot race their completion.
A host without its own process group is rejected before readiness.

## Cooperative attach for a native Python worker

Install the core AW Python SDK wheel (`anolisa-aw-sdk`), the optional Claude
adapter wheel (`anolisa-aw-sdk-claude`), and Claude Agent SDK
0.2.88 in the application's Python environment (legacy `aw_sdk.claude` imports
forward to `aw_sdk_claude` when the optional adapter wheel is installed); the worker and Service run as
the same user. Compile a descriptor with the `claude-resident-hooks` profile at
deployment so the worker starts without the configuration parser:

```bash
(
  set -e
  staged=$(mktemp)
  trap 'rm -f "$staged"' EXIT
  aw config descriptor --config /absolute/aw.yaml --agent claude \
    --profile claude-resident-hooks > "$staged"
  install -m 0400 "$staged" /absolute/aw-claude.descriptor.json
)
```

A descriptor with a nonempty `allow_plaintext_http_hosts` list must have all write permissions cleared.

`ResidentClaude.from_config()` reads the same configuration and takes the
target, Service path and input protection from the descriptor. Without
`descriptor_file` it runs the `aw config show` offline parser instead. The
profile version `resident-hooks/v1` identifies the Hook contract and does not
change with the Claude SDK package version.

This entry always requires AW and does not support `native_fallback`. It reuses
the resident Bridge's fail-closed cleanup: when the SDK cannot confirm an
interruption, drain times out or cleanup state is unknown, AW sends SIGKILL to
the worker's process group, including its native children. Start a standalone
worker with `setsid python worker.py`. For Gunicorn, call `os.setsid()` from
`post_fork` and create the factory in each worker after fork, so termination
never reaches the master or sibling workers. A process without its own session
is rejected when the factory is created. Deploy workers as replaceable units,
for example restarted by Gunicorn.

Call `startup()` on the worker event loop during application startup. A new
client waits for AW Ready at `connect()`, before starting the native SDK.
The default wait is 5 seconds and `attach_timeout_ms` can change it. The optional
`listen(automatic=True)` attaches when the configured Service is available.

```python
from aw_sdk_claude import ResidentClaude
from claude_agent_sdk import ClaudeAgentOptions

async def startup():
    worker = ResidentClaude.from_config(
        "/absolute/aw.yaml",
        target="claude",
        descriptor_file="/absolute/aw-claude.descriptor.json",
    )
    worker.listen()
    return worker

async def ask(worker, prompt):
    async with worker.create_client(ClaudeAgentOptions()) as client:
        await client.query(prompt)
        async for message in client.receive_response():
            yield message

async def shutdown(worker):
    await worker.close()
```

Set `WORKER_PID` to the worker that called `startup()`. The `aw connect` reply
returns an `application_id`; set `AW_SDK_INSTANCE` to that ID and `AW_SOCKET`
to the descriptor's `endpoint` for `aw disconnect`.
Drain response iterators and disconnect clients before disconnecting or shutting
down. Each open client keeps its policy through disconnect, and `aw disconnect`
returns busy while a client owns an AW Task.

```bash
aw connect --config /absolute/aw.yaml --sdk-pid "$WORKER_PID"
aw disconnect --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
```

For managed in-process MCP handlers, use
`aw_sdk_claude.create_sdk_mcp_server` in the client's options. Catch
`aw_sdk_claude.SubmissionDenied` around a guarded query. The cooperative
factory admits `input.submit` (when configured), `tool.before` with
`replace_input` and `tool.after` with `replace_result`. The final tool-result
security boundary uses the separate foreground entrypoint.

## Check user text before submission

Use [the Claude submission configuration](../../../../../src/aw/crates/aw-service/examples/aw.claude-submission.yaml)
to enable `scan_prompt` and `scan_user_input`. Replace the executable/socket paths,
start sec-core separately, and keep the application's existing options. The example
selects the resident entrypoint; a foreground worker can use its existing argv.
The default configuration rejects risks and scan failures before native submission.
Catch `aw_submission.SubmissionDenied` around `query` or resident `connect(prompt)`;
after this exception returns, the same connected client can accept new input.
A rejected input has no response stream to consume.

| Factory path with checks enabled | Supported input |
| --- | --- |
| Foreground `async with create_client(options)` then `query` | String or one-message async iterable; one client per worker |
| Resident `connect(prompt)` and `query` | String or one-message async iterable; clients keep their conversation between drained turns |
| Async message content | A string or nonempty list of `{type: "text", text: "..."}` blocks |

Async envelopes must have `type: "user"`, a `message` containing only `role: "user"`
and `content`, and optional `session_id`, `uuid`, and null `parent_tool_use_id`.
AW consumes and closes the iterator before scanning. Multiple messages, media,
unknown fields, blank text and inputs exceeding 256 KiB of encoded AW input are
rejected. Session labels must be nonblank and at most 512 UTF-8 bytes. Removing
`input.submit` preserves the previously supported native input modes.

All steps share one immutable snapshot, turn identity and service event budget.
Text blocks are joined with newlines for scanning; the native message retains
its original blocks. Strings are forwarded unchanged. Input checks run before
native `UserPromptSubmit` Hooks; those Hooks, native vetoes and settings remain
installed. Their added context or later rewrites are outside this input snapshot.
The scan covers fresh user text, not the full conversation or final model prompt.

Audited, recoverable Provider faults follow `on_error`: `block` rejects this input;
`report` records the fault and continues while the event budget remains usable.
Risk blocking applies with either failure policy. Protocol/identity/scope faults,
audit failure and uncertain cleanup stop further submissions. Cancellation waits
for scanner/event cleanup before propagating and allows another turn when cleanup
succeeds. Drain each accepted response before another query; do not receive while
input admission is running. Concurrent disconnect cancels and joins admission.

Native `connect(str)` with `can_use_tool` retains the SDK restriction; connect first
without a prompt and then call `query`, or supply the supported async message.

Resident clients retire input records after each acknowledged check and retain a
monotonic sequence to reject replay. They can continue beyond 1,024 submissions
without recreating the client. Foreground bindings retain a cumulative 1,024-claim
limit shared with legacy tool events; create a fresh binding before that limit.
Active input events and scanner calls are released after each check.

## Task observation

With `evidence` enabled, each acknowledged managed session owns a bounded observation
worker. Configured native Hook calls contribute callback metadata; session close drains
pending reads and releases its source-session claims. A failed evidence query does not
block an otherwise admitted tool call. Pi and Claude SDK retain the existing source
identity limits: callback association remains `unverified`, coverage remains `unknown`,
and AW does not claim that AgentSight history belongs to this SDK session. Empty tool
policies do not add synthetic observation callbacks for these adapters.
