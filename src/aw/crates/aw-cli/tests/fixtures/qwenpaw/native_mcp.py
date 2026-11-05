"""Real MCP protocol calls through native Toolkit and QwenPaw Coordinator."""

from native_resident_support import *

import mcp.types as types
from mcp.server.lowlevel import Server
from mcp.shared.memory import create_connected_server_and_client_session
from agentscope.mcp import MCPClient, HttpMCPConfig
from agentscope.tool import Toolkit
from agentscope.message import ToolResultState
from aw_resident_test.aw_mcp import managed_mcp_tools


class NativeMcpTests(NativeFixture):
    @asynccontextmanager
    async def fixture(self, *, hooks=None, mutate=None, result=None, handler=None):
        server = Server("fixture")
        calls = []
        descriptor = types.Tool(name="read.record", description="Read a synthetic record",
            inputSchema={"type": "object", "properties": {"value": {"type": "integer"}},
                         "required": ["value"], "$defs": {"row": {"type": "string"}}},
            annotations=types.ToolAnnotations(readOnlyHint=True, idempotentHint=True))
        envelope = result or types.CallToolResult(content=[types.TextContent(type="text", text="native")],
                                                 structuredContent={"value": 3}, isError=True)

        @server.list_tools()
        async def listing():
            return [descriptor]

        @server.call_tool()
        async def call(name, arguments):
            calls.append((name, arguments))
            if handler:
                await handler()
            return envelope

        async with create_connected_server_and_client_session(server) as session:
            client = MCPClient(name="fixture", is_stateful=True,
                               mcp_config=HttpMCPConfig(url="http://fixture.invalid/mcp"))
            client._session, client._is_connected = session, True
            tools = await managed_mcp_tools(client)
            tool = tools[0]
            toolkit = Toolkit(tools=tools)
            async def dispatch(tool_call):
                if mutate:
                    mutate(tool_call, tool)
                async for chunk in agent.toolkit.call_tool(tool_call, agent.state):
                    yield chunk
            mw, agent, coordinator = self.setup_agent(dispatch, toolkit=toolkit, hooks=hooks)
            native_call = ToolCallBlock(id="model-parent", name=tool.name, input='{"value":3}')
            try:
                yield SimpleNamespace(calls=calls, descriptor=descriptor, envelope=envelope,
                    tool=tool, mw=mw, agent=agent, coordinator=coordinator, call=native_call)
            finally:
                await coordinator.shutdown()

    async def test_actual_client_preserves_schema_annotations_and_complete_error(self):
        hooks = [{"event": event, "step": event, "on_error": "report", "budget_ms": 1000}
                 for event in ("tool.before", "tool.after")]
        async with self.fixture(hooks=hooks) as f:
            results = await self.request(f.mw, f.agent, [f.call])
            result = next(item for item in results if isinstance(item, ToolResponse))
            self.assertEqual(f.tool.input_schema["$defs"], f.descriptor.inputSchema["$defs"])
            self.assertEqual(f.tool._tool.annotations, f.descriptor.annotations)
            self.assertEqual(f.calls, [("read.record", {"value": 3})])
            self.assertEqual(result.state, ToolResultState.ERROR)
            self.assertEqual(result.metadata["aw_mcp_result"],
                             f.envelope.model_dump(mode="json", by_alias=True))
            self.assertEqual(len(f.mw.scope.callbacks), 2)
            for _, payload in f.mw.scope.callbacks:
                self.assertEqual(payload["aw_mcp_context"], {
                    "server": "fixture", "tool": "read.record", "subject": f.tool.name,
                    "parent_tool_call_id": "model-parent", "generation": 1,
                    "identity_scope": "model_tool"})
            post = f.mw.scope.callbacks[-1][1]["tool_response"]
            self.assertEqual(post["metadata"]["aw_mcp_result"]["structuredContent"], {"value": 3})

    async def test_pre_denial_reaches_zero_actual_tools_call(self):
        hooks = [{"event": "tool.before", "step": "policy", "on_error": "block", "budget_ms": 1000}]
        async with self.fixture(hooks=hooks) as f:
            async def deny(hook, payload):
                return 2, b"fixture denied"
            f.mw.scope.invoke = deny
            await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(f.calls, [])

    async def test_native_argument_or_descriptor_mutation_prevents_remote_execution(self):
        for field in ("arguments", "tool", "server", "schema"):
            with self.subTest(field=field):
                def mutate(call, tool):
                    if field == "arguments":
                        call.input = '{"value":4}'
                    elif field == "tool":
                        tool._tool.name = "other"
                    elif field == "server":
                        tool.mcp_name = "other"
                    else:
                        tool.input_schema["properties"] = {}
                async with self.fixture(mutate=mutate) as f:
                    with self.assertRaises(Exception):
                        await self.request(f.mw, f.agent, [f.call])
                    self.assertEqual(f.calls, [])
                    self.assertTrue(f.mw.failed)

    async def test_multimodal_envelope_and_native_rendering_survive(self):
        envelope = types.CallToolResult(content=[
            types.TextContent(type="text", text="caption"),
            types.ImageContent(type="image", data="aW1hZ2U=", mimeType="image/png"),
            types.AudioContent(type="audio", data="YXVkaW8=", mimeType="audio/wav"),
        ], structuredContent={"opaque": [1, True, None]}, isError=False)
        async with self.fixture(result=envelope) as f:
            results = await self.request(f.mw, f.agent, [f.call])
            result = next(item for item in results if isinstance(item, ToolResponse))
            self.assertEqual(len(result.content), 3)
            self.assertEqual(result.metadata["aw_mcp_result"], envelope.model_dump(mode="json", by_alias=True))

    async def test_missing_or_retired_parent_never_reaches_remote(self):
        async with self.fixture() as f:
            with self.assertRaisesRegex(ResidentFailure, "lacks native"):
                await f.tool.call(value=3)
            self.assertEqual(f.calls, [])
            await self.request(f.mw, f.agent, [f.call])
            with self.assertRaisesRegex(ResidentFailure, "lacks native"):
                await f.tool.call(value=3)
            self.assertEqual(len(f.calls), 1)

    async def test_cancellation_retires_call_without_publishing_result(self):
        entered, released = asyncio.Event(), asyncio.Event()
        async def pending():
            entered.set()
            try:
                await asyncio.Event().wait()
            finally:
                released.set()
        async with self.fixture(handler=pending) as f:
            task = asyncio.create_task(self.request(f.mw, f.agent, [f.call]))
            await asyncio.wait_for(entered.wait(), 2)
            await asyncio.wait_for(f.agent.close(), 3)
            outcome = (await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 2))[0]
            self.assertIsInstance(outcome, (asyncio.CancelledError, ResidentFailure))
            self.assertEqual(len(f.calls), 1)
            self.assertTrue(f.mw.failed)
            self.assertEqual(f.mw.active, {})
            self.assertTrue(f.mw.scope.closed)
        # Remote work is owned by the server; closing its transport joins it.
        self.assertTrue(released.is_set())

    async def test_concurrent_agents_keep_same_arguments_and_parent_isolated(self):
        async with self.fixture() as first, self.fixture() as second:
            results = await asyncio.gather(self.request(first.mw, first.agent, [first.call]),
                                           self.request(second.mw, second.agent, [second.call]))
            self.assertEqual([len(first.calls), len(second.calls)], [1, 1])
            self.assertTrue(all(any(isinstance(item, ToolResponse) for item in row) for row in results))
            self.assertTrue(first.mw.scope.closed and second.mw.scope.closed)

    async def test_required_post_failure_withholds_structured_result(self):
        hooks = [{"event": "tool.after", "step": "policy", "on_error": "withhold_result", "budget_ms": 1000}]
        async with self.fixture(hooks=hooks) as f:
            async def fail(hook, payload):
                self.assertIn("aw_mcp_result", payload["tool_response"]["metadata"])
                return 1, b""
            f.mw.scope.invoke = fail
            with self.assertRaises(Exception):
                await self.request(f.mw, f.agent, [f.call])
            self.assertEqual(len(f.calls), 1)
            self.assertTrue(f.mw.failed)
            self.assertTrue(f.mw.scope.closed)


if __name__ == "__main__":
    unittest.main()
