# Claude SDK

[中文版](../../../zh/aw/agents/claude-sdk.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start a Claude SDK worker

Use Claude Agent SDK 0.2.88 with its bundled CLI 2.1.161. The Adapter launches one
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
target/debug/aw status --config /absolute/aw.claude-sdk.yaml
target/debug/aw stop --config /absolute/aw.claude-sdk.yaml
```

Claude SDK event budgets must be 1..55,000 ms. Each `tool.before` or `tool.after`
route uses `events.<name>.budget_ms` when set, otherwise
`execution.default_event_budget_ms`. `aw run` enforces this adapter-specific
limit after static configuration validation.

The worker's options cwd must equal the launch cwd. Readiness is published after
the real SDK initialization handshake accepts callbacks. AW observes or denies
`PreToolUse`; `PostToolUse` and `PostToolUseFailure` both supply after observations.
Denied tools do not produce a synthetic after. Existing hook vetoes and native
permissions remain effective. Existing pre-hook `updatedInput` and permission
`updated_input` changes are denied, because concurrent callbacks cannot establish
that AW inspected the final arguments. When `tool.before` is active, nonempty
`PermissionRequest` matchers are rejected before initialization; use supported
`PreToolUse` or `can_use_tool` callbacks instead. Caller-specified
`permission_prompt_tool_name` is also rejected with `tool.before`; SDK-owned stdio
permissions for the wrapped `can_use_tool` remain supported. Caller `cli_path` overrides and missing
bundled binaries are rejected. The worker must not detach or start multiple
SDK clients/tasks within one launcher binding; Gunicorn and business deployments
need their own task lifecycle integration.

CLI 2.1.161 emits `PostToolUse` for SDK MCP `isError: true` responses and omits that
flag from callback data. After observation therefore does not classify every
business failure. Native Bash execution failure does use `PostToolUseFailure`.
See the [Adapter contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/en/adapters/claude-sdk-adapter.md)
for identity, cancellation and validation limits.

The options factory requires `setting_sources=[]`, no `settings` override and no
CLI plugins. `extra_args` allows only `strict-mcp-config: None`; the SDK native
`strict_mcp_config` option is also preserved. Model/authentication options are
unchanged. Applications which discover project skills through settings sources
need an explicit integration design before using this restricted first bridge.
The direct task-worker mode permits one client per worker. Use the [resident entrypoint](../deployment/resident.md) for multiple task clients.

The `claude-sdk` adapter supports this restricted foreground worker factory;
`aw agent connect` does not provide persistent installation for it, and all `--native-*`
overrides are unsupported.

### Enable Bash PreTool and PostTool optimization

Use the [shared Tokenless configuration](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml),
select its `claude` target, and replace the absolute executable, worker and state
paths. The source `claude-worker.py` example explicitly exposes and allows native
Bash in addition to its echo tool. Keep your application's own permission policy
when composing its factory. See the [Tokenless guide](../../../../user-guide/en/aw/tokenless.md) for Provider
installation, RTK requirements and statistics settings.

PreTool blocks the original Bash attempt and returns retry arguments to the model;
the new call passes all policies and native permissions again. PostTool replaces
only completed, non-image, non-interrupted Bash stdout. Put completion observers
and custom steps before the single final `replace_result` step. They see the
original result; stderr and other native fields are retained. Failure/report keeps
the original result. MCP tools on this native callback path remain observable but are not compressed because
the native callback does not expose reliable business-error status.

Run pinned native tests through [contributing](../../../../../src/aw/CONTRIBUTING.md).
See [resident deployment](../deployment/resident.md) for resident clients and managed MCP.
