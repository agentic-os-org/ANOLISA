"""Resident native callback recovery retains identity, cleanup and failure fences."""
import asyncio
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import AsyncMock

sys.path.insert(0, str(Path(sys.argv.pop(1))))
sdk = ModuleType("claude_agent_sdk")
for name in ("ClaudeAgentOptions", "ClaudeSDKClient", "HookMatcher", "PermissionResultDeny", "ResultMessage"):
    setattr(sdk, name, type(name, (), {}))
sys.modules["claude_agent_sdk"] = sdk
sys.modules["anyio"] = SimpleNamespace()
sys.modules["aw_scope"] = SimpleNamespace(Scope=object)
sys.modules["aw_model"] = SimpleNamespace(ModelRelay=object)
sys.modules["aw_transport"] = SimpleNamespace(ManagedTransport=object)

from aw_claude import CallbackFailure
from aw_invocations import Invocations, dispatch
from aw_resident import ResidentBridge


class Contracts(unittest.IsolatedAsyncioTestCase):
    def bridge(self):
        bridge = object.__new__(ResidentBridge)
        bridge.config = {"hooks": [{"event": "tool.after", "step": "compression",
            "on_error": "report", "replace_result": True}]}
        bridge.scope = SimpleNamespace(check=lambda **kwargs: None, failed=False,
            invocation_limit=8, register=AsyncMock(), finish=AsyncMock())
        bridge.failure = None
        bridge.mcp_names = set()
        bridge.invocations = Invocations(bridge)
        bridge.stop_task = AsyncMock()
        bridge._transport = AsyncMock(return_value=(0, b"{}"))
        return bridge

    async def call(self, bridge, mutation=None):
        ledger = bridge.invocations
        ledger.begin()
        entry = ledger.produce("Bash")
        arguments = {"command": "echo original"}
        await ledger.register(entry, arguments)
        entry.phase = "allowed"
        payload = {"hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_use_id": entry.call_id, "tool_input": arguments,
            "tool_response": {"stdout": "original", "stderr": "", "interrupted": False}}
        ticket = ledger.reserve("after", entry.call_id, "callback")
        token = dispatch.set(ticket)
        try:
            if mutation:
                mutation(payload)
            async with ledger.callback():
                result = await bridge.callback(payload, entry.call_id, {})
            return entry, result
        finally:
            dispatch.reset(token)

    async def test_neutral_report_finishes_and_reuses_the_execution(self):
        bridge = self.bridge()
        for _ in range(2):
            entry, result = await self.call(bridge)
            self.assertEqual(result, {})
            self.assertEqual(entry.phase, "adopted")
            await bridge.invocations.drain()
            self.assertFalse(bridge.invocations.active)
            self.assertFalse(bridge.invocations.pending)
            self.assertIsNone(bridge.failure)
        bridge.stop_task.assert_not_called()
        self.assertEqual(bridge.scope.finish.await_count, 2)

    async def test_unclassified_callback_failures_still_stop(self):
        for fault in (CallbackFailure("fixture"), TimeoutError(), OSError("fixture")):
            with self.subTest(fault=type(fault).__name__):
                bridge = self.bridge()
                bridge._transport.side_effect = fault
                _, reply = await self.call(bridge)
                self.assertFalse(reply["continue_"])
                self.assertIsNotNone(bridge.failure)
                bridge.stop_task.assert_awaited_once()

    async def test_native_response_corruption_still_stops(self):
        for code, output in ((1, b""), (0, b"invalid"),
                             (0, b'{"hookSpecificOutput":{"updatedToolOutput":{"stdout":"changed"}}}')):
            with self.subTest(output=output):
                bridge = self.bridge()
                bridge._transport.return_value = code, output
                _, reply = await self.call(bridge)
                self.assertFalse(reply["continue_"])
                bridge.stop_task.assert_awaited_once()

    async def test_changed_identity_or_arguments_stop_before_transport(self):
        for change in (lambda p: p.update(tool_use_id="stale"),
                       lambda p: p.update(tool_input={"command": "changed"}),
                       lambda p: p.update(tool_name="Read")):
            bridge = self.bridge()
            _, reply = await self.call(bridge, change)
            self.assertFalse(reply["continue_"])
            bridge._transport.assert_not_called()
            bridge.stop_task.assert_awaited_once()

    async def test_scope_failure_still_stops(self):
        bridge = self.bridge()
        def invalidate(payload):
            def check(**kwargs):
                raise RuntimeError("expired scope")
            bridge.scope.check = check
            bridge.scope.failed = True
        _, reply = await self.call(bridge, invalidate)
        self.assertFalse(reply["continue_"])
        bridge._transport.assert_not_called()
        bridge.stop_task.assert_awaited_once()

    async def test_finish_failure_stops_and_retains_unacknowledged_entry(self):
        bridge = self.bridge()
        bridge.scope.finish.side_effect = RuntimeError("cleanup failed")
        entry, reply = await self.call(bridge)
        self.assertFalse(reply["continue_"])
        self.assertEqual(entry.phase, "closing")
        self.assertIn(entry.call_id, bridge.invocations.active)
        bridge.stop_task.assert_awaited_once()

    async def test_cancellation_is_not_report_recovery(self):
        bridge = self.bridge()
        bridge._transport.side_effect = asyncio.CancelledError()
        with self.assertRaises(asyncio.CancelledError):
            await self.call(bridge)
        bridge.scope.finish.assert_not_called()
        self.assertTrue(bridge.invocations.active)
        self.assertFalse(bridge.invocations.pending)


if __name__ == "__main__":
    unittest.main()
