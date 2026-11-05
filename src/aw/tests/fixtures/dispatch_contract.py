"""Exercise pinned SDK stdout parsing and cancellation before/after callback entry."""

import asyncio
from importlib.metadata import version
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

import anyio
from claude_agent_sdk import ClaudeAgentOptions, PermissionResultAllow
from claude_agent_sdk._internal.query import Query

AW = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(AW / "crates/aw-adapter/native/claude-sdk"))
from aw_invocations import Invocations
from aw_resident import ManagedClient
from aw_transport import ManagedTransport


class Process:
    returncode = 0

    async def wait(self):
        return self.returncode


class DispatchTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.assertEqual(version("claude-agent-sdk"), "0.2.88")
        scope = SimpleNamespace(invocation_limit=4, check=lambda: None,
                                register=AsyncMock(), finish=AsyncMock())
        self.bridge = SimpleNamespace(scope=scope, failure=None, mcp_servers={"managed"},
                                      stop_task=AsyncMock(), request_stop=lambda: None)
        self.bridge.failure_reply = lambda *args: setattr(self.bridge, "failure", "failed")
        self.ledger = self.bridge.invocations = Invocations(self.bridge)
        self.ledger.begin()
        self.transport = ManagedTransport(ClaudeAgentOptions(), self.bridge)
        self.sender, receiver = anyio.create_memory_object_stream[str](8)
        self.transport.delegate._stdout_stream = receiver
        self.transport.delegate._process = Process()
        self.transport.delegate.write = AsyncMock()
        self.query = Query(self.transport, is_streaming_mode=True)
        self.client = SimpleNamespace(
            _aw_completion=asyncio.Lock(), _aw_task_started=True, _aw_query_finished=False,
            _aw_relay=SimpleNamespace(drain=AsyncMock()), _aw_bridge=self.bridge,
        )
        self.entered, self.cleaning, self.release = (asyncio.Event() for _ in range(3))
        self.hold = False
        self.calls = 0

        async def callback(*args):
            async with self.ledger.callback():
                self.calls += 1
                self.entered.set()
                if self.hold:
                    try:
                        await asyncio.Event().wait()
                    finally:
                        self.cleaning.set()
                        await self.release.wait()
                return {}

        async def permission(*args):
            await callback(*args)
            return PermissionResultAllow()

        self.query.hook_callbacks["custom"] = callback
        self.query.can_use_tool = permission
        self.query._handle_sdk_mcp_request = callback
        self.receiver = self.query.receive_messages()
        self.addAsyncCleanup(self.cleanup, receiver)
        await self.query.start()

    async def cleanup(self, receiver):
        self.release.set()
        async with asyncio.timeout(3):
            await self.query.close()
            await asyncio.gather(*(task.wait() for task in list(self.query._child_tasks)))
            await self.receiver.aclose()
        self.query.close_receive_stream()
        await self.sender.aclose()
        await receiver.aclose()
        self.assertFalse(self.query._inflight_requests)
        self.assertFalse(self.query._child_tasks)

    async def frames(self, *messages):
        marker = {"type": "system", "subtype": "fixture_barrier"}
        # One stdout chunk is parsed without a checkpoint between request and
        # cancel. The real Query creates and cancels its handler before entry.
        await self.sender.send("\n".join(json.dumps(m) for m in (*messages, marker)) + "\n")
        self.assertEqual(await asyncio.wait_for(anext(self.receiver), 1), marker)
        async with asyncio.timeout(1):
            while any(task.done() for task in self.query._child_tasks):
                await asyncio.sleep(0)

    async def request(self, kind):
        entry = self.ledger.produce("tool")
        await self.ledger.register(entry, {})
        data = {"subtype": kind, "tool_use_id": entry.call_id, "input": {}}
        if kind == "hook_callback":
            data["callback_id"] = "custom"
        elif kind == "can_use_tool":
            data["tool_name"] = "tool"
        else:
            data.update(server_name="managed", message={
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tool", "arguments": {},
                           "_meta": {"claudecode/toolUseId": entry.call_id}},
            })
        return {"type": "control_request", "request_id": entry.call_id, "request": data}

    async def complete(self):
        with patch("aw_resident.fail_stop") as fail_stop:
            await asyncio.wait_for(ManagedClient._aw_complete(self.client), 1)
            fail_stop.assert_not_called()
        self.assertFalse(self.ledger.pending)
        self.assertFalse(self.ledger.active)
        self.assertIsNone(self.bridge.failure)

    def begin(self):
        self.ledger.begin()
        self.client._aw_task_started = True
        self.client._aw_query_finished = False

    async def cancelled_before_entry(self, kind):
        request = await self.request(kind)
        cancel = {"type": "control_cancel_request", "request_id": request["request_id"]}
        await self.frames(request, cancel)
        self.assertFalse(self.entered.is_set())
        self.assertFalse(self.query._inflight_requests)
        self.assertFalse(self.query._child_tasks)
        await self.complete()
        self.begin()
        await self.frames(await self.request(kind))
        await asyncio.wait_for(self.entered.wait(), 1)
        await self.complete()
        self.assertEqual(self.calls, 1)
        self.assertEqual(self.bridge.scope.finish.await_count, 2)

    async def test_hook_cancel_in_same_chunk_drains_and_reuses_query(self):
        await self.cancelled_before_entry("hook_callback")

    async def test_permission_cancel_in_same_chunk_drains_and_reuses_query(self):
        await self.cancelled_before_entry("can_use_tool")

    async def test_mcp_cancel_in_same_chunk_drains_and_reuses_query(self):
        await self.cancelled_before_entry("mcp_message")

    async def test_entered_callback_keeps_async_cleanup_barrier(self):
        self.hold = True
        request = await self.request("hook_callback")
        await self.frames(request)
        await asyncio.wait_for(self.entered.wait(), 1)
        await self.frames({"type": "control_cancel_request", "request_id": request["request_id"]})
        await asyncio.wait_for(self.cleaning.wait(), 1)
        completing = asyncio.create_task(self.complete())
        try:
            async with asyncio.timeout(1):
                while not self.ledger.closing:
                    await asyncio.sleep(0)
            self.assertTrue(self.ledger.pending)
            self.assertFalse(completing.done())
            with self.assertRaises(RuntimeError):
                self.ledger.begin()
        finally:
            self.release.set()
            await completing
        self.begin()
        self.hold = False
        await self.frames(await self.request("hook_callback"))
        await self.complete()
        self.assertEqual(self.calls, 2)


if __name__ == "__main__":
    unittest.main()
