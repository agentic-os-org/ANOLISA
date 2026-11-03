"""Structured MCP identity crosses the actual AW scope, Host and Provider."""

import argparse
import json
from pathlib import Path
import sys
import unittest

import native_mcp
from native_resident_support import NativeFixture, ToolResponse
import native_resident_service


class NativeMcpServiceTests(unittest.IsolatedAsyncioTestCase):
    command = native_resident_service.NativeResidentServiceTests.command
    rpc = native_resident_service.NativeResidentServiceTests.rpc
    asyncSetUp = native_resident_service.NativeResidentServiceTests.asyncSetUp
    stop = native_resident_service.NativeResidentServiceTests.stop
    fixture = native_mcp.NativeMcpTests.fixture
    request = NativeFixture.request

    def setup_agent(self, *args, **kwargs):
        middleware, agent, coordinator = NativeFixture.setup_agent(self, *args, **kwargs)
        middleware.scope = self.middleware.scope
        middleware.config = self.middleware.config
        return middleware, agent, coordinator

    async def run_mcp(self, deny):
        async with self.fixture(hooks=self.middleware.config["hooks"]) as f:
            f.call.input = json.dumps({"value": 3, "deny": deny})
            results = await self.request(f.mw, f.agent, [f.call])
            terminal = next(item for item in results if isinstance(item, ToolResponse))
            self.assertEqual(len(f.calls), 0 if deny else 1)
            self.assertEqual(terminal.state.value, "denied" if deny else "error")
            before = json.loads((self.root / "tool.before-provider.json").read_text())
            after = json.loads((self.root / "tool.after-provider.json").read_text())
            context = before["native"]["aw_mcp_context"]
            self.assertEqual(context, after["native"]["aw_mcp_context"])
            self.assertEqual(context["server"], "fixture")
            self.assertEqual(context["tool"], "read.record")
            self.assertEqual(context["parent_tool_call_id"], "model-parent")
            self.assertEqual(context["identity_scope"], "model_tool")
            self.assertEqual(before["tool"]["input"], {"value": 3, "deny": deny})
            self.assertEqual(before["tool"]["call_id"], after["tool"]["call_id"])
            self.assertNotEqual(before["tool"]["call_id"], "model-parent")
            if not deny:
                envelope = after["tool"]["result"]["metadata"]["aw_mcp_result"]
                self.assertEqual(envelope["structuredContent"], {"value": 3})
                self.assertTrue(envelope["isError"])
            recent = await self.rpc("recent_executions")
            invokes = [item for item in recent["items"] if item["method"] == "invoke"]
            self.assertEqual(len(invokes), 2)
            await self.rpc("release_instance", instance_id=self.parent)
            self.assertEqual((await self.rpc("status"))["instances"], [])

    async def test_provider_denial_prevents_actual_mcp_call(self):
        await self.run_mcp(True)

    async def test_provider_receives_structured_identity_and_original_error(self):
        await self.run_mcp(False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    args, remaining = parser.parse_known_args()
    NativeMcpServiceTests.binary = args.bin.resolve() / "aw"
    unittest.main(argv=[sys.argv[0], *remaining])
