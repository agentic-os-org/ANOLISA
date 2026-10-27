# Deploy resident SDK applications

[中文版](../../../zh/aw/deployment/resident.md)

Configure task factories, MCP clients and cleanup for long-running SDK hosts.

## Resident Pi and Claude SDK hosts

Install matching AW, Providers and pinned SDK packages in the image. Use the
[shared configuration example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.resident-sdk.yaml),
replace its paths and select the target:

```bash
aw run --config /etc/aw/aw.yaml --agent claude
aw run --config /etc/aw/aw.yaml --agent pi
```

For Claude, set the target argv to the application's Python interpreter followed
by `-m aw_claude -- -m application`. A foreground Gunicorn host can use
`-m aw_claude -- -m gunicorn -c conf/gunicorn.py main:app`; create task clients
inside workers after fork. In the common client factory:

```python
from aw_claude import create_client

client = create_client(options)
try:
    await client.connect()
    await client.query(prompt)
    async for message in client.receive_response():
        yield message
finally:
    await client.disconnect()
```

Sequential queries reuse a client after its response and callbacks drain; each
client retains its conversation. Resume/session-store options remain available.
Concurrent work uses independent clients with separate cwd/env.
Each query accepts a string or a finite one-message AsyncIterable; multi-message
input is rejected before dispatch. Submit successive turns with sequential queries.
Both `receive_response()` and `receive_messages()` drain callbacks before
delivering `ResultMessage`. EOF without that message raises an error; keep
`disconnect()` in `finally` so unfinished queries still receive an interrupt.

For in-process MCP tools, also import `create_sdk_mcp_server` from `aw_claude` and
create a fresh server for each client. Keep SDK `tool` definitions and business closures.
The [managed MCP contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/en/adapters/claude-mcp.md) covers single successful text replacement, reliable errors and explicit correlation.

Resident Pi and Claude hosts reject enabled AgentSight evidence before publishing
readiness. Set evidence `mode: off` for resident hosts; legacy task mode continues
to support evidence.

For Pi, export the existing SDK session factory and a resident host entrypoint:

```javascript
export async function createSession({ sdk, cwd }) {
  return sdk.createAgentSession({ cwd /* application model/tools/options */ });
}
export async function runWorker({ withSession }) {
  await serveRequests(async (request) => {
    return withSession({ cwd: request.workspace, environment: request.environment },
      async (session) => session.prompt(request.prompt));
  });
}
```

`serveRequests` is the application's server. Install business callbacks before
returning from the factory. Close sessions at task completion; concurrent prompts
on one session are rejected. Avoid creating unmanaged SDK clients elsewhere.

The example composes custom Hooks, sec-core Bash scanning and Tokenless PreTool/
PostTool. Start the sec-core daemon separately and supply a real custom Hook.
Opaque MCP tools need explicit security semantics. Resident mode stops tasks on
callback errors even for `on_error: report`; policy denial remains a normal native
denial. A failed Claude interrupt terminates AW's owned foreground process group,
so configure supervisor restart. Drain requests before sending termination and
set the target's `shutdown_grace_ms` to allow time for the application's remaining
cleanup. The outer launcher defaults to a two-second grace from its first signal
forwarding until forced cleanup starts. See
[shutdown grace](lifecycle.md)
for scope and limits. Schema/BeforeModel and
Retrieve require the request/effect admission and authorization work listed in the
[resident contract](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/en/adapters/resident-sdk.md).

## Use one security policy for resident SDKs

The [shared SDK security example](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
selects Qoder SDK or QwenPaw through `--agent`. Replace every absolute path,
initialize the native runtime as described in its section, and start sec-core
with the configured socket before launching:

```bash
aw validate --config /absolute/path/aw.sdk-security.yaml
aw run --config /absolute/path/aw.sdk-security.yaml --agent qoder-sdk -- "your prompt"
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home aw run \
  --config /absolute/path/aw.sdk-security.yaml --agent qwenpaw
```

The sec-core step matches only `native:qoder-sdk:Bash` and
`native:qwenpaw:execute_shell_command`. AW projects the selected target's steps
before capability admission and Provider preparation. Other tools still use the
custom before/after checks without acquiring an implicit sec-core scan. Add
explicit selectors and matching Provider tool mappings when extending coverage.

The custom scripts use the boolean Hook contract: read one normalized event from
stdin and print a JSON boolean with exit status zero. In this example, before
`true` requests `block`; after `true` records `observe`. After `false` produces no
effect and is still a successful check. A failed result check must exit nonzero;
`on_error: withhold_result` then prevents delivery and subsequent dispatch.
Native permissions and the caller's daemon/App lifecycle remain in force.
Use ordinary `aw run` and explicit idle updates between closed Queries or
completed replies. Private credentials need
the [explicit execution factory](../../../../../src/aw/docs/design/en/adapters/private-execution.md)
in application code; this YAML installs the security policy.
