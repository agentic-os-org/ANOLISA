"""Pre-dispatch submission and cleanup contracts without an installed Claude SDK."""

import asyncio
from contextlib import nullcontext
import json
import os
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest

sys.path.insert(0, sys.argv.pop(1))


class ResultMessage:
    pass


class Client:
    def __init__(self, options=None):
        self.options = options

    async def query(self, prompt, session_id="default"):
        if isinstance(prompt, str):
            self.sent.append((prompt, session_id))
        else:
            async for message in prompt:
                self.sent.append((message, session_id))

    async def receive_messages(self):
        yield ResultMessage()

    async def connect(self, prompt=None):
        self.connected = True

    async def disconnect(self):
        self.connected = False


sdk = ModuleType("claude_agent_sdk")
sdk.ClaudeSDKClient = Client
sdk.ResultMessage = ResultMessage
sdk.ClaudeAgentOptions = sdk.HookMatcher = sdk.PermissionResultDeny = sdk.Transport = object
sys.modules["claude_agent_sdk"] = sdk
sys.modules["claude_agent_sdk._internal.transport.subprocess_cli"] = SimpleNamespace(
    SubprocessCLITransport=object,
)
sys.modules["anyio"] = SimpleNamespace(CancelScope=lambda **kwargs: nullcontext())
sys.modules["aw_model"] = SimpleNamespace(ModelRelay=object)

from aw_claude import CallbackFailure
from aw_foreground import CheckedClient
from aw_invocations import Invocations
from aw_resident import ManagedClient
from aw_submission import SubmissionDenied, prepare


async def nothing(*args):
    pass


def stream(*messages):
    async def inputs():
        for message in messages:
            yield message
    return inputs()


def message(text="original", **fields):
    return {"type": "user", "message": {"role": "user", "content": text}, **fields}


class Bridge:
    def __init__(self):
        self.config = {"hooks": [
            {"event": "input.submit", "step": step, "on_error": "block",
             "budget_ms": 1000, "replace_result": False} for step in ("prompt", "pii")
        ]}
        self.calls = []
        self.failure = None
        self.turn = 0
        self.scope = SimpleNamespace(check=lambda: None, next_submission=self.next_submission)
        self.invocations = Invocations(self)
        self.response = (0, b"")
        self.entered = asyncio.Event()
        self.wait = None
        self.close_wait = None
        self.closing = asyncio.Event()
        self.close_response = (0, b"")
        self.cancel_close = False

    async def next_submission(self):
        self.turn += 1
        return str(self.turn)

    def failure_reply(self, *args):
        self.failure = "protocol failed"

    async def stop_task(self):
        raise AssertionError("pre-dispatch denial must not interrupt the SDK")

    async def _transport(self, hook, payload):
        self.calls.append((dict(hook), dict(payload)))
        if hook.get("close_event"):
            self.closing.set()
            if self.close_wait:
                await self.close_wait.wait()
            if self.cancel_close:
                raise asyncio.CancelledError
            return self.close_response
        self.entered.set()
        if self.wait:
            await self.wait.wait()
        return self.response


def client(resident):
    bridge = Bridge()
    if resident:
        value = ManagedClient.__new__(ManagedClient)
        value._aw_check = lambda: None
        value._aw_task_started = False
        value._aw_query_finished = False
        value._aw_completion = asyncio.Lock()
        value._aw_relay = SimpleNamespace(drain=nothing)
    else:
        value = CheckedClient(SimpleNamespace(cwd="/work"), bridge)
    value._aw_bridge = bridge
    value._aw_cwd = "/work"
    value._aw_state = "connected"
    value._aw_receiving = False
    value._aw_input_task = None
    value.sent = []
    if resident:
        value._aw_pid = os.getpid()
        value._aw_lock = asyncio.Lock()
        value._aw_connect_task = None
        value._aw_mcp = []
        value._aw_relay.close = nothing
        bridge.scope.close = nothing
    return value


class Contracts(unittest.IsolatedAsyncioTestCase):
    async def test_strings_and_async_text_keep_original_payload(self):
        for resident in (False, True):
            value = client(resident)
            await value.query(" 中文 \n", "session-one")
            self.assertEqual(value.sent, [(" 中文 \n", "session-one")])
            calls = value._aw_bridge.calls
            self.assertEqual(len(calls), 3)
            self.assertEqual(len({p["aw_turn_id"] for _, p in calls}), 1)
            self.assertTrue(calls[-1][0]["close_event"])
            async for _ in value.receive_response():
                pass
            original = message([{"type": "text", "text": "甲"}, {"type": "text", "text": "乙"}],
                               session_id="session-two", uuid="user-message")
            await value.query(stream(original))
            self.assertEqual(value.sent[-1], (original, "default"))
            self.assertEqual(calls[-1][1]["input_text"], "甲\n乙")
            self.assertEqual(calls[-1][1]["session_id"], "session-two")
            self.assertNotEqual(calls[0][1]["aw_turn_id"], calls[-1][1]["aw_turn_id"])
            self.assertNotIn("tool_use_id", calls[-1][1])

    async def test_denied_turn_releases_client_without_starting_invocations(self):
        for resident in (False, True):
            value = client(resident)
            value._aw_bridge.response = (2, b'"risk"')
            with self.assertRaisesRegex(SubmissionDenied, "risk"):
                await value.query("rejected")
            self.assertEqual(value.sent, [])
            self.assertIsNone(value._aw_input_task)
            self.assertIsNone(value._aw_bridge.failure)
            self.assertFalse(value._aw_bridge.invocations.running)
            self.assertEqual(len(value._aw_bridge.calls), 2)
            value._aw_bridge.response = (0, b"")
            await value.query("recovered")
            self.assertEqual(value.sent, [("recovered", "default")])

    async def test_cancellation_waits_for_cleanup_and_next_query_succeeds(self):
        for resident in (False, True):
            for phase in ("scan", "allowed", "denied"):
                for cleanup in ("ok", "error", "cancelled"):
                    with self.subTest(resident=resident, phase=phase, cleanup=cleanup):
                        value = client(resident)
                        bridge = value._aw_bridge
                        bridge.close_wait = asyncio.Event()
                        if phase == "scan":
                            bridge.wait = asyncio.Event()
                        elif phase == "denied":
                            bridge.response = (2, b'"risk"')
                        bridge.close_response = (1 if cleanup == "error" else 0, b"")
                        bridge.cancel_close = cleanup == "cancelled"
                        task = asyncio.create_task(value.query("cancelled"))
                        try:
                            if phase == "scan":
                                await bridge.entered.wait()
                                task.cancel()
                            await bridge.closing.wait()
                            for _ in range(3):
                                task.cancel()
                                await asyncio.sleep(0.01)
                                self.assertFalse(task.done())
                                self.assertIs(value._aw_input_task, task)
                                with self.assertRaises(RuntimeError):
                                    await value.query("overlap")
                                with self.assertRaises(RuntimeError):
                                    await anext(value.receive_messages())
                                self.assertEqual(value.sent, [])
                        finally:
                            bridge.close_wait.set()
                            result = await asyncio.gather(task, return_exceptions=True)
                        self.assertIsInstance(result[0],
                            asyncio.CancelledError if cleanup == "ok" else CallbackFailure)
                        self.assertIsNone(value._aw_input_task)
                        self.assertFalse(bridge.invocations.running)
                        self.assertFalse(bridge.invocations.active or bridge.invocations.pending)
                        self.assertEqual(value.sent, [])
                        bridge.wait = None
                        bridge.response = bridge.close_response = (0, b"")
                        bridge.cancel_close = False
                        if cleanup == "ok":
                            await value.query("after cancellation")
                            self.assertEqual(len(value.sent), 1)
                        else:
                            with self.assertRaises(RuntimeError):
                                await value.query("uncertain cleanup remains fenced")

    async def test_disconnect_cancels_and_joins_input_cleanup(self):
        for resident in (False, True):
            value = client(resident)
            bridge = value._aw_bridge
            bridge.wait = asyncio.Event()
            bridge.close_wait = asyncio.Event()
            query = asyncio.create_task(value.query("cancel on disconnect"))
            await bridge.entered.wait()
            closing = asyncio.create_task(value.disconnect())
            await bridge.closing.wait()
            self.assertFalse(closing.done())
            self.assertEqual(value._aw_state, "closing")
            self.assertIs(value._aw_input_task, query)
            bridge.close_wait.set()
            await asyncio.wait_for(closing, 1)
            self.assertTrue(query.cancelled())
            self.assertEqual(value._aw_state, "closed")
            self.assertIsNone(value._aw_input_task)
            self.assertFalse(bridge.invocations.running)
            self.assertFalse(bridge.invocations.active or bridge.invocations.pending)
            self.assertEqual(value.sent, [])

    async def test_protocol_and_cleanup_faults_fence_future_submissions(self):
        for resident in (False, True):
            for response, close_response in [((1, b""), (0, b"")),
                    ((0, b'{}'), (0, b"")), ((2, b'{}'), (0, b"")),
                    ((2, b'"deny"'), (1, b""))]:
                value = client(resident)
                value._aw_bridge.response = response
                value._aw_bridge.close_response = close_response
                with self.assertRaises(CallbackFailure):
                    await value.query("unsafe")
                self.assertEqual(value.sent, [])
                self.assertIsNone(value._aw_input_task)
                with self.assertRaises(RuntimeError):
                    await value.query("still fenced")

    async def test_input_iteration_failure_and_cancellation_release_occupancy(self):
        for resident in (False, True):
            for cancel in (False, True):
                value = client(resident)
                entered = asyncio.Event()
                closed = []
                async def inputs():
                    try:
                        yield message()
                        entered.set()
                        if cancel:
                            await asyncio.Event().wait()
                        raise ValueError("iterator failed")
                    finally:
                        closed.append(True)
                task = asyncio.create_task(value.query(inputs()))
                await entered.wait()
                if cancel:
                    task.cancel()
                with self.assertRaises(asyncio.CancelledError if cancel else ValueError):
                    await task
                self.assertEqual(closed, [True])
                self.assertEqual(value._aw_bridge.calls, [])
                self.assertIsNone(value._aw_input_task)
                await value.query("recovered")
                self.assertEqual(len(value.sent), 1)

    async def test_unsupported_input_never_enters_provider_or_model(self):
        cases = ["", " \n\t", "x" * (256 * 1024), stream(),
                 stream(message(), message()), stream(message([{"type": "image", "data": "x"}])),
                 stream(message("text", parent_tool_use_id="call")),
                 stream(message("text", session_id="")), stream(message("text", extra="hidden")),
                 stream({"type": "user", "message": {"role": "assistant", "content": "x"}})]
        for prompt in cases:
            value = client(True)
            with self.assertRaises((ValueError, TypeError)):
                await value.query(prompt)
            self.assertEqual(value._aw_bridge.calls, [])
            self.assertEqual(value.sent, [])
            await value.query("recovered")

    async def test_missing_and_overlong_identity_is_rejected(self):
        for identity in (None, "", " ", "x" * 513, "中" * 171):
            with self.assertRaises(SubmissionDenied):
                await prepare("text", identity, "/work", 1000)

    async def test_input_snapshot_cannot_be_changed_after_scan(self):
        value = client(True)
        original = message("original")
        value._aw_bridge.wait = asyncio.Event()
        task = asyncio.create_task(value.query(stream(original)))
        await value._aw_bridge.entered.wait()
        original["message"]["content"] = "changed"
        value._aw_bridge.wait.set()
        await task
        self.assertEqual(value.sent[0][0]["message"]["content"], "original")
        self.assertEqual(value._aw_bridge.calls[0][1]["input_text"], "original")

    async def test_without_checks_resident_keeps_native_input_shapes(self):
        value = client(True)
        value._aw_bridge.config["hooks"] = []
        original = message([{"type": "image", "data": "native-owned"}])
        await value.query(stream(original))
        self.assertEqual(value.sent, [(original, "default")])
        self.assertEqual(value._aw_bridge.calls, [])

    async def test_foreground_connect_prompt_is_checked_and_denial_is_recoverable(self):
        value = client(False)
        value._aw_state = "new"
        value._aw_bridge.response = (2, b'"risk"')
        with self.assertRaises(SubmissionDenied):
            await value.connect("blocked")
        self.assertTrue(value.connected)
        value._aw_bridge.response = (0, b"")
        await value.query("recovery")
        self.assertEqual(value.sent, [("recovery", "default")])


if __name__ == "__main__":
    unittest.main()
