"""Deterministic iterator ownership tests without a native CLI or installed SDK."""

import asyncio
from contextlib import aclosing
from dataclasses import dataclass
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest

sys.path.insert(0, str(Path(sys.argv.pop(1))))


class CallbackFailure(RuntimeError):
    pass


class ProcessError(RuntimeError):
    pass


class ResultMessage:
    pass


@dataclass
class Options:
    can_use_tool: object = None


class Client:
    async def receive_messages(self):
        try:
            for message in self.sdk_messages:
                yield message
        finally:
            await asyncio.sleep(0)
            self.sdk_closed = True


class Delegate:
    def __init__(self, **kwargs):
        self.messages = [{"type": "result"}, {"type": "result"}]
        self.iterator = None
        self.stopped = False
        self.connects = 0
        self.finalized = False
        self.waits = 0
        self.failure = None
        self.finally_failure = None
        self.entered = asyncio.Event()
        self.release = None
        self.finalize_entered = asyncio.Event()
        self.finalize_release = None

    async def connect(self):
        self.connects += 1

    def read_messages(self):
        self.iterator = self.read()
        return self.iterator

    async def read(self):
        try:
            try:
                for message in self.messages:
                    if self.release is not None:
                        self.entered.set()
                        await self.release.wait()
                    yield message
            except GeneratorExit:
                # SDK 0.2.88 treats GeneratorExit as EOF and waits for the CLI.
                pass
            self.waits += 1
            if self.failure:
                raise self.failure
        finally:
            if self.finalize_release is not None:
                self.finalize_entered.set()
                await self.finalize_release.wait()
            await asyncio.sleep(0)
            self.finalized = True
            if self.finally_failure:
                raise self.finally_failure

    async def close(self):
        self.stopped = True


sdk = ModuleType("claude_agent_sdk")
sdk.ClaudeAgentOptions, sdk.Transport = Options, object
sdk.ClaudeSDKClient, sdk.ResultMessage = Client, ResultMessage
sdk.PermissionResultDeny = type("PermissionResultDeny", (), {})
sys.modules["claude_agent_sdk"] = sdk
sys.modules["claude_agent_sdk._internal.transport.subprocess_cli"] = SimpleNamespace(
    SubprocessCLITransport=Delegate,
)
sys.modules["aw_claude"] = SimpleNamespace(
    CallbackFailure=CallbackFailure, Bridge=object, _deny=None,
)
sys.modules["anyio"] = SimpleNamespace()
sys.modules["aw_scope"] = SimpleNamespace(Scope=object)
sys.modules["aw_model"] = SimpleNamespace(ModelRelay=object)

from aw_invocations import dispatch
from aw_transport import ManagedTransport
from aw_resident import ManagedClient


class Contracts(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.stops = []
        self.bridge = SimpleNamespace(
            mcp_servers=set(),
            failure_reply=lambda *args: self.stops.append("failure"),
            request_stop=lambda: self.stops.append("stop"),
            invocations=SimpleNamespace(reserve=lambda *args: "ticket"),
        )
        self.transport = ManagedTransport(Options(), self.bridge)
        self.readers = []

    async def asyncTearDown(self):
        # Clean the intentionally failing baseline without relying on GC.
        for reader in self.readers:
            try:
                await reader.athrow(asyncio.CancelledError())
            except (asyncio.CancelledError, StopAsyncIteration):
                pass
        iterator = self.transport.delegate.iterator
        if iterator is not None:
            try:
                await iterator.athrow(asyncio.CancelledError())
            except (asyncio.CancelledError, StopAsyncIteration):
                pass

    def reader(self):
        reader = self.transport.read_messages()
        self.readers.append(reader)
        return reader

    async def test_closed_transport_rejects_reconnect_before_native_launch(self):
        await self.transport.connect()
        reader = self.reader()
        self.assertIs(self.transport.read_messages(), reader)
        await anext(reader)
        await self.transport.close()
        with self.assertRaisesRegex(RuntimeError, "cannot reconnect"):
            await self.transport.connect()
        self.assertEqual(self.transport.delegate.connects, 1)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            self.transport.read_messages()
        self.assertIsNone(reader.ag_frame)
        fresh = ManagedTransport(Options(), self.bridge)
        try:
            await fresh.connect()
            new_reader = fresh.read_messages()
            self.assertIsNot(new_reader, reader)
            self.assertEqual(await anext(new_reader), {"type": "result"})
        finally:
            await fresh.close()
        self.assertEqual(fresh.delegate.connects, 1)
        self.assertTrue(fresh.delegate.finalized)

    async def test_closing_transport_rejects_reconnect_before_finalizer_returns(self):
        await self.transport.connect()
        reader = self.reader()
        await asyncio.create_task(anext(reader))
        self.transport.delegate.finalize_release = asyncio.Event()
        closing = asyncio.create_task(self.transport.close())
        try:
            await asyncio.wait_for(self.transport.delegate.finalize_entered.wait(), 1)
            self.assertFalse(closing.done())
            with self.assertRaisesRegex(RuntimeError, "cannot reconnect"):
                await self.transport.connect()
            with self.assertRaisesRegex(RuntimeError, "closed"):
                self.transport.read_messages()
            self.assertEqual(self.transport.delegate.connects, 1)
        finally:
            self.transport.delegate.finalize_release.set()
            await asyncio.wait_for(closing, 1)

    async def test_close_without_reader_also_prevents_reconnect(self):
        await self.transport.close()
        with self.assertRaisesRegex(RuntimeError, "cannot reconnect"):
            await self.transport.connect()
        self.assertEqual(self.transport.delegate.connects, 0)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            self.transport.read_messages()

    async def test_close_owns_reader_after_native_consumer_exits(self):
        reader = self.reader()
        await asyncio.create_task(anext(reader))
        await self.transport.close()
        self.assertTrue(self.transport.delegate.finalized)
        self.assertTrue(self.transport.delegate.stopped)
        self.assertEqual(self.transport.delegate.waits, 0)
        self.assertIsNone(reader.ag_frame)
        self.assertIsNone(dispatch.get())
        self.assertEqual(self.stops, [])

    async def test_foreign_close_never_reads_another_frame(self):
        reader = self.reader()
        await asyncio.create_task(anext(reader))
        token = dispatch.set("foreign-task-ticket")
        try:
            await reader.aclose()
            self.assertEqual(dispatch.get(), "foreign-task-ticket")
        finally:
            dispatch.reset(token)
        self.assertTrue(self.transport.delegate.finalized)
        self.assertEqual(self.transport.delegate.waits, 0)
        self.assertEqual(self.stops, [])

    async def test_callback_task_copies_ticket_and_reader_restores_context(self):
        self.transport.delegate.messages = [{"type": "control_request", "request_id": "r",
            "request": {"subtype": "can_use_tool", "tool_use_id": "call"}},
            {"type": "result"}]
        reader = self.reader()
        await anext(reader)

        async def callback():
            return dispatch.get()

        task = asyncio.create_task(callback())
        await anext(reader)
        self.assertIsNone(dispatch.get())
        self.assertEqual(await task, "ticket")
        await reader.aclose()
        self.assertIsNone(dispatch.get())

    async def test_consumer_error_is_not_reinterpreted_as_callback_failure(self):
        reader = self.reader()
        await anext(reader)
        failure = RuntimeError("consumer failed")
        with self.assertRaises(RuntimeError) as caught:
            await reader.athrow(failure)
        self.assertIs(caught.exception, failure)
        self.assertEqual(self.stops, [])
        self.assertTrue(self.transport.delegate.finalized)

    async def test_invalid_identity_stops_then_keeps_interrupt_reader_available(self):
        self.transport.delegate.messages = [
            {"type": "system", "subtype": "init", "claude_code_version": "invalid"},
            {"type": "control_response"},
        ]
        reader = self.reader()
        self.assertEqual(await anext(reader), {"type": "control_response"})
        self.assertEqual(self.stops, ["failure", "stop"])
        await reader.aclose()

    async def test_pending_read_cancellation_joins_delegate_finalizer(self):
        self.transport.delegate.release = asyncio.Event()
        reader = self.reader()
        task = asyncio.create_task(anext(reader))
        await self.transport.delegate.entered.wait()
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(self.transport.delegate.finalized)
        await self.transport.close()
        self.assertTrue(self.transport.delegate.stopped)

    async def test_natural_native_exit_error_remains_visible(self):
        failure = ProcessError("native exit 1")
        self.transport.delegate.failure = failure
        self.transport.delegate.messages = []
        with self.assertRaises(ProcessError) as caught:
            await anext(self.reader())
        self.assertIs(caught.exception, failure)
        self.assertEqual(self.transport.delegate.waits, 1)
        await self.transport.close()

    async def test_close_error_is_visible_and_still_closes_native_process(self):
        reader = self.reader()
        await anext(reader)
        failure = RuntimeError("delegate finalizer failed")
        self.transport.delegate.finally_failure = failure
        with self.assertRaises(RuntimeError) as caught:
            await self.transport.close()
        self.assertIs(caught.exception, failure)
        self.assertTrue(self.transport.delegate.stopped)
        with self.assertRaisesRegex(RuntimeError, "cannot reconnect"):
            await self.transport.connect()
        self.assertEqual(self.transport.delegate.connects, 0)

    async def test_unrelated_cancellation_is_not_swallowed(self):
        reader = self.reader()
        await anext(reader)
        failure = asyncio.CancelledError("external cleanup cancellation")
        self.transport.delegate.finally_failure = failure
        with self.assertRaises(asyncio.CancelledError) as caught:
            await self.transport.close()
        self.assertIs(caught.exception, failure)
        self.assertTrue(self.transport.delegate.stopped)

    async def test_message_consumers_close_sdk_stream_before_returning(self):
        for consumer in ("receive_messages", "receive_response"):
            for failure in (None, "callback failed"):
                with self.subTest(consumer=consumer, failure=failure):
                    client = ManagedClient.__new__(ManagedClient)
                    client._aw_check = lambda: None
                    client._aw_state = "connected"
                    client._aw_receiving = False
                    client._aw_task_started = True
                    client._aw_query_finished = False
                    client.sdk_messages = [ResultMessage()]
                    client.sdk_closed = False

                    async def complete():
                        client._aw_query_finished = True

                    client._aw_complete = complete
                    client._aw_bridge = SimpleNamespace(failure=failure, stop_task=complete)
                    async with aclosing(getattr(client, consumer)()) as messages:
                        if failure:
                            with self.assertRaises(CallbackFailure):
                                await anext(messages)
                        else:
                            self.assertIsInstance(await anext(messages), ResultMessage)
                    self.assertTrue(client.sdk_closed)
                    self.assertFalse(client._aw_receiving)


if __name__ == "__main__":
    unittest.main()
