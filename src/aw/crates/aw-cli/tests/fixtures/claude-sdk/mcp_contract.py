"""Exercise managed handler ownership without installing a native SDK or model."""
import asyncio
from dataclasses import dataclass
import importlib.util
import json
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest


def copy_json(value):
    return json.loads(json.dumps(value))


@dataclass
class SdkMcpTool:
    name: str
    description: str
    input_schema: dict
    handler: object
    annotations: object = None


class Server:
    def __init__(self, tools):
        self.tools = tools


sdk = ModuleType("claude_agent_sdk")
sdk.SdkMcpTool = SdkMcpTool
sdk.create_sdk_mcp_server = lambda name, version, tools: {
    "type": "sdk", "name": name, "instance": Server(tools),
}
bridge_module = ModuleType("aw_claude")
bridge_module.CallbackFailure = type("CallbackFailure", (RuntimeError,), {})
bridge_module._copy_json = copy_json
sys.modules.update(claude_agent_sdk=sdk, aw_claude=bridge_module)
spec = importlib.util.spec_from_file_location("aw_mcp", Path(sys.argv.pop(1)))
mcp = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = mcp
spec.loader.exec_module(mcp)


class Scope:
    def __init__(self, deny=False, hold=False):
        self.handle = {"binding": {"instance_id": "task"}}
        self.deny, self.hold = deny, hold
        self.events = []
        self.entered, self.release = asyncio.Event(), asyncio.Event()

    def check(self):
        pass

    async def invoke(self, hook, payload):
        self.events.append(copy_json(payload))
        if (payload["event"] == "tool.before" and self.deny
                and (payload["server"], payload["tool_name"]) == ("local", "touch")):
            return 2, b'"Blocked by policy"'
        if payload["event"] == "tool.after" and self.hold:
            self.entered.set()
            await asyncio.wait_for(self.release.wait(), 2)
        return 0, b"{}"


def claim(tool, scope):
    managed = mcp.create_sdk_mcp_server("business", tools=[tool])
    bridge = SimpleNamespace(scope=scope, failure=None, config={"hooks": [
        {"event": name, "replace_result": False} for name in ("tool.before", "tool.after")
    ]})
    managed.claim("local", bridge)
    return managed, managed["instance"].tools[0].handler


class HandlerOwnershipTests(unittest.IsolatedAsyncioTestCase):
    async def test_claim_metadata_cannot_redirect_registered_policy(self):
        calls = []

        async def business(arguments):
            calls.append(arguments)
            return {"content": [{"type": "text", "text": "business"}]}

        scope = Scope(deny=True)
        managed, invoke = claim(SdkMcpTool("touch", "Business closure", {}, business), scope)
        owner = managed.bridge
        for name, value in (("key", "allowed_alias"), ("bridge", object()),
                            ("claimed", False), ("names", ("allowed_alias",))):
            with self.subTest(name=name), self.assertRaises(AttributeError):
                setattr(managed, name, value)
        self.assertEqual(managed.key, "local")
        self.assertIs(managed.bridge, owner)
        self.assertTrue((await invoke({}))["is_error"])
        self.assertEqual(calls, [])
        self.assertEqual(scope.events[0]["server"], "local")
        managed.release()
        self.assertIsNone(managed.bridge)
        self.assertTrue(managed.claimed)
        with self.assertRaises(ValueError):
            managed.claim("retry", owner)

    async def test_batch_failure_preserves_fresh_server_and_existing_owner(self):
        fresh = mcp.create_sdk_mcp_server("fresh")
        owned = mcp.create_sdk_mcp_server("owned")
        owner = SimpleNamespace(mcp_names=set())
        mcp.claim_servers({"owned": owned}, owner)
        failed = SimpleNamespace(mcp_names=set())
        with self.assertRaises(ValueError):
            mcp.claim_servers({"fresh": fresh, "owned": dict(owned)}, failed)
        self.assertFalse(fresh.claimed)
        self.assertIsNone(fresh.bridge)
        self.assertIs(owned.bridge, owner)
        self.assertEqual(failed.mcp_names, set())
        retry = SimpleNamespace(mcp_names=set())
        self.assertEqual(mcp.claim_servers({"fresh": fresh}, retry), [fresh])
        self.assertIs(fresh.bridge, retry)

    async def test_invalid_batch_never_consumes_a_claim(self):
        for alias in ("second", "ambiguous__key", "invalid key"):
            with self.subTest(alias=alias):
                fresh = mcp.create_sdk_mcp_server("fresh")
                bridge = SimpleNamespace(mcp_names=set())
                with self.assertRaises(ValueError):
                    mcp.claim_servers({"first": fresh, alias: dict(fresh)}, bridge)
                self.assertFalse(fresh.claimed)
                self.assertIsNone(fresh.bridge)
                self.assertEqual(bridge.mcp_names, set())
                self.assertEqual(mcp.claim_servers({"local": fresh}, bridge), [fresh])

    async def test_original_tool_rename_cannot_bypass_registered_policy(self):
        calls = []

        async def business(arguments):
            calls.append(arguments)
            return {"content": [{"type": "text", "text": "business"}]}

        original = SdkMcpTool("touch", "Business closure", {}, business)
        scope = Scope(deny=True)
        managed, invoke = claim(original, scope)
        original.name = "allowed_alias"
        result = await invoke({"value": "fixture"})
        self.assertEqual(managed.names, ("touch",))
        self.assertEqual(managed["instance"].tools[0].name, "touch")
        self.assertEqual(calls, [])
        self.assertTrue(result["is_error"])
        self.assertEqual([event["tool_name"] for event in scope.events], ["touch"])

    async def test_original_handler_assignment_keeps_the_registered_closure(self):
        calls = []

        async def original_handler(arguments):
            calls.append(("original", arguments))
            return {"content": [{"type": "text", "text": "original"}]}

        async def other_handler(arguments):
            calls.append(("other", arguments))
            return {"content": [{"type": "text", "text": "other"}]}

        original = SdkMcpTool("touch", "Business closure", {}, original_handler)
        managed, invoke = claim(original, Scope())
        original.handler = other_handler
        result = await invoke({"value": "fixture"})
        self.assertEqual(calls, [("original", {"value": "fixture"})])
        self.assertEqual(result["content"][0]["text"], "original")
        self.assertIsNot(managed["instance"].tools[0], original)

    async def test_shared_results_deliver_the_checked_snapshot(self):
        for mode in ("plain", "error", "multi", "structured"):
            with self.subTest(mode=mode):
                shared = {"content": [{"type": "text", "text": "checked"}],
                          "is_error": mode == "error"}
                if mode == "multi":
                    shared["content"].append({"type": "text", "text": "second"})
                if mode == "structured":
                    shared["structuredContent"] = {"value": "checked"}
                expected = copy_json(shared)

                async def business(arguments):
                    return shared

                scope = Scope(hold=True)
                managed, invoke = claim(SdkMcpTool("touch", "Shared result", {}, business), scope)
                pending = asyncio.create_task(invoke({}))
                try:
                    await asyncio.wait_for(scope.entered.wait(), 2)
                    shared["content"][0]["text"] = "unverified"
                    shared["is_error"] = not shared["is_error"]
                    if mode == "structured":
                        shared["structuredContent"]["value"] = "unverified"
                    scope.release.set()
                    delivered = await asyncio.wait_for(pending, 2)
                    self.assertEqual(scope.events[-1]["tool_result"], expected)
                    self.assertEqual(delivered, expected)
                    self.assertIsNot(delivered, shared)
                finally:
                    scope.release.set()
                    if not pending.done():
                        pending.cancel()
                    await asyncio.gather(pending, return_exceptions=True)
                    managed.release()


if __name__ == "__main__":
    unittest.main()
