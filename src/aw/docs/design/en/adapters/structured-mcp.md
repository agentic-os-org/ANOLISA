# Structured MCP dispatch

[中文版](../../zh/adapters/structured-mcp.md)

The managed SDK boundaries authorize the effective MCP arguments before an
in-process handler or remote `tools/call` runs. Policies use ordinary
`tool.before` and `tool.after` steps and tool selectors. Configure code scanning
and remote authorization as separate steps when both are required. The adapter
adds no implicit Provider invocation.

## QwenPaw 2.2.2.post1 / AgentScope 2.0.10

After AW's `aw-native` plugin has loaded, an application can resolve a connected
native `MCPClient` into guarded tools:

```python
from agentscope.tool import Toolkit
from plugin_aw_native.aw_mcp import managed_mcp_tools

tools = await managed_mcp_tools(client)
toolkit = Toolkit(tools=tools)
```

Give this Toolkit to the Agent assembled by the existing managed Agent factory.
The application owns the native client's connection and close lifecycle.
The helper resolves a fixed tool list; refresh it explicitly when discovery
changes. Dynamic `Toolkit(mcps=[client])` discovery retains its existing native
behavior and does not install this actual-call boundary.

The helper calls the actual `MCPClient.list_tools()` / `get_tool()` implementation,
preserves the full input schema and annotations, and retains native permission
checks. The resident captures the selected tool before PreTool. The MCP tool
compares its descriptor, connection, parsed arguments and active invocation again
immediately before `ClientSession.call_tool()`. Each invocation permits one MCP
call. Missing identity, changed arguments or descriptor, repeated entry, cancelled
or retired ownership fail closed. Stateless clients create their native session
inside the same guarded operation. Stateful clients keep the existing session.

`native.aw_mcp_context` contains `server`, the original MCP `tool`, model-facing
`subject`, `parent_tool_call_id`, the scope-local invocation sequence in
`generation`, and `identity_scope: model_tool`. It derives from the actual native
tool and resident context. A sanitized model-facing name is never parsed back into
a server or tool name. The ordinary resident executes Pre/Post once; the MCP
boundary validates dispatch without invoking either phase again.

Native rendering retains text/media and error status. The complete MCP
`CallToolResult`, including `structuredContent`, `isError`, content blocks and
extension fields, is also retained in `ToolResponse.metadata.aw_mcp_result`.
This envelope passes through the existing lease redactor and Post barrier.
Selected `withhold_result` failures stop delivery and subsequent dispatch.
Cancellation joins the local client call through the native Coordinator;
the remote server owns cancellation and cleanup of its own operation.

## Qoder SDK 1.0.56 / Worker 1.1.78

Use the existing `createSdkMcpServer()` export from AW's managed Qoder bridge.
The factory accepts `parentIdentity: "query"` (default) or `"required"`.
`required` rejects before native server creation because the pinned Worker does
not transmit model-parent identity at its MCP dispatch boundary.

The Worker sender passes `{subtype: "mcp_message", server_name, message}` to the
control channel. `SDKControlMcpMessageRequest` has the same fields, and the SDK
`QueryRunner.processControlRequest()` forwards `message` into the in-process MCP
transport. These contracts establish Query ownership and the actual server,
tool and arguments. They carry no model tool ID. A parent-aware integration needs
a trusted Worker-produced identity at this exact dispatch seam, bound to the
registered native model invocation and generation. Equal arguments, command
spelling and dispatch order cannot supply that evidence.

`native.aw_mcp_context` explicitly reports `identity_scope: query`,
`parent_tool_call_id: null`, `parent_query_id` and the execution `generation`.
AW registers a fresh child invocation, snapshots approved arguments, verifies
native parser dispatch and schema output, and consumes each handler ticket once.
The handler function is captured at factory construction. Native MCP schemas,
annotations, structured errors and multimodal result envelopes retain SDK
semantics. Results are copied before Post so later mutation cannot replace the
checked result. The resident checks cancellation and generation before execution
and delivery, and joins original handler finalizers before retiring its ledger.

For managed tools, native AW policy callbacks delegate to this actual MCP
boundary. Application native Hooks and permissions still run. Each configured
AW phase executes once at the actual call. Unmanaged server/tool contexts keep
normal native policy callbacks. Delegation trusts the pinned Worker's Hook
`mcp_context.server_name` / `tool_name` to identify managed tools; those fields
are native metadata, separate from model-supplied tool arguments. The subsequent
control request must still pass the actual-call identity and argument checks.
Changing this native metadata contract requires revalidating both boundaries.
An installed `resident.executionOwner` wraps
the original handler Promise with a private execution lease and redacts its
complete result before Post; the Query owner supplies and closes that owner.

## Acceptance

`qwenpaw/native_mcp.py` exercises actual MCP protocol sessions, native Toolkit
parsing and QwenPaw Coordinator behavior: denial with zero server calls, mutation,
parallel ownership, cancellation, complete structured errors and multimedia.
`qwenpaw/native_mcp_service.py` adds real AW scope/Host/Provider calls and checks
that identity and the original envelope reach both phases exactly once.
`qoder_sdk/parser.test.mjs` uses the real pinned SDK parser and MCP server with a
fixture transport for identity, denial, schema, cancellation and generation
checks. The repository native matrix automates hosted Qoder daemon/Hook, reload
and strict Post tests. Hosted managed-MCP allow, denial, strict Post and
cancellation currently have separate manual container evidence recorded in the
PR; they are not scheduled matrix cases. Re-run those four cases on the deployment
candidate, checking native Hook delegation, actual handler entry, private-result
redaction and original finalizer drain. Promoting them into the native matrix
remains follow-up work under #9071.
