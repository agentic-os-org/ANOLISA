"""Credential-free producer identity, replay and callback drain contracts."""

import asyncio
import json
from pathlib import Path
import sys
import types
import unittest

AW = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(AW / "crates/aw-adapter/native/claude-sdk"))


class CallbackFailure(RuntimeError):
    pass


# Isolate the protocol tests from the optional installed SDK package. The
# production invocation/stream modules are exercised unchanged below.
sys.modules["aw_claude"] = types.SimpleNamespace(CallbackFailure=CallbackFailure)
from aw_invocations import Invocations, dispatch
from aw_model_stream import MessageStream, rewrite_message


class Scope:
    invocation_limit = 2

    def __init__(self):
        self.registered = 0
        self.finished = 0

    def check(self):
        pass

    async def register(self, sequence, call, name, arguments):
        self.registered += 1
        assert sequence == self.registered

    async def finish(self, sequence, call):
        self.finished += 1


class Contracts(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.scope = Scope()
        self.ledger = Invocations(types.SimpleNamespace(scope=self.scope, failure=None))
        self.ledger.begin()

    async def issue(self):
        entry = self.ledger.produce("tool")
        await self.ledger.register(entry, {"value": 1})
        return entry

    async def phase(self, entry, phase):
        ticket = self.ledger.reserve(f"{entry.call_id}-{phase}", entry.call_id, phase)
        token = dispatch.set(ticket)
        try:
            async with self.ledger.callback():
                self.ledger.claim(entry.call_id, "tool", {"value": 1}, phase)
                if phase == "before":
                    entry.phase = "allowed"
                else:
                    await self.ledger.finish(entry)
        finally:
            dispatch.reset(token)

    async def retire(self, entry):
        await self.ledger.check_model_request(
            {"messages": [{"content": [{"type": "tool_result", "tool_use_id": entry.call_id}]}]}
        )

    async def test_sparse_active_identity_survives_1200_retirements(self):
        slow = await self.issue()
        for _ in range(1200):
            entry = await self.issue()
            await self.phase(entry, "before")
            await self.phase(entry, "after")
            await self.retire(entry)
            self.assertEqual(len(self.ledger.active), 1)
            with self.assertRaises(CallbackFailure):
                self.ledger.reserve("late", entry.call_id, "before")
        await self.phase(slow, "before")
        await self.phase(slow, "after")
        await self.retire(slow)
        self.assertEqual(self.scope.finished, 1201)
        self.assertFalse(self.ledger.active)

    async def test_duplicate_dispatch_and_changed_input_are_rejected(self):
        entry = await self.issue()
        await self.phase(entry, "before")
        with self.assertRaises(CallbackFailure):
            self.ledger.reserve("new-envelope", entry.call_id, "before")
        ticket = self.ledger.reserve("after", entry.call_id, "after")
        token = dispatch.set(ticket)
        try:
            async with self.ledger.callback():
                with self.assertRaises(CallbackFailure):
                    self.ledger.claim(entry.call_id, "tool", {"value": 2}, "after")
        finally:
            dispatch.reset(token)

    async def test_post_before_pre_and_model_before_adoption_fail_closed(self):
        entry = await self.issue()
        with self.assertRaises(CallbackFailure):
            await self.phase(entry, "after")
        with self.assertRaises(CallbackFailure):
            await self.retire(entry)
        await self.ledger.drain()
        with self.assertRaises(CallbackFailure):
            self.ledger.reserve("late", entry.call_id, "before")

    async def test_native_denial_retires_without_fabricating_tool_completion(self):
        entry = await self.issue()
        ticket = self.ledger.reserve("native-denial", entry.call_id, "permission")
        token = dispatch.set(ticket)
        try:
            async with self.ledger.callback():
                self.ledger.native_denial()
        finally:
            dispatch.reset(token)
        await self.ledger.check_model_request(
            {
                "messages": [
                    {
                        "content": [
                            {"type": "tool_result", "tool_use_id": entry.call_id, "is_error": True}
                        ]
                    }
                ]
            }
        )
        self.assertFalse(self.ledger.active)
        self.assertEqual(self.scope.finished, 1)
        with self.assertRaises(CallbackFailure):
            self.ledger.reserve("late", entry.call_id, "before")

    async def test_cancelled_callback_cleanup_blocks_execution_transition(self):
        entry = await self.issue()
        ticket = self.ledger.reserve("hook", entry.call_id, "custom")
        entered, cleanup, release = asyncio.Event(), asyncio.Event(), asyncio.Event()

        async def callback():
            token = dispatch.set(ticket)
            try:
                async with self.ledger.callback():
                    entered.set()
                    try:
                        await asyncio.Event().wait()
                    finally:
                        cleanup.set()
                        await release.wait()
            finally:
                dispatch.reset(token)

        task = asyncio.create_task(callback())
        await entered.wait()
        task.cancel()
        await cleanup.wait()
        draining = asyncio.create_task(self.ledger.drain())
        try:
            await asyncio.sleep(0)
            self.assertFalse(draining.done())
            with self.assertRaises(CallbackFailure):
                self.ledger.begin()
        finally:
            release.set()
            await asyncio.gather(task, return_exceptions=True)
            await draining
        self.ledger.begin()
        self.assertFalse(self.ledger.active)
        with self.assertRaises(CallbackFailure):
            self.ledger.reserve("old-generation", entry.call_id, "custom")

    async def test_concurrent_model_requests_retire_shared_history_once(self):
        for denied in (False, True):
            entry = await self.issue()
            await self.phase(entry, "before")
            if denied:
                entry.native_denied = True
            else:
                await self.phase(entry, "after")
            ticket = self.ledger.reserve("custom", entry.call_id, "custom")
            entered, release = asyncio.Event(), asyncio.Event()

            async def callback():
                token = dispatch.set(ticket)
                try:
                    async with self.ledger.callback():
                        entered.set()
                        await release.wait()
                finally:
                    dispatch.reset(token)

            task = asyncio.create_task(callback())
            await entered.wait()
            started = [asyncio.Event(), asyncio.Event()]

            async def request(index):
                started[index].set()
                await self.retire(entry)

            requests = [asyncio.create_task(request(index)) for index in range(2)]
            try:
                await asyncio.wait_for(asyncio.gather(*(event.wait() for event in started)), 1)
                self.assertTrue(all(not request.done() for request in requests))
            finally:
                release.set()
                await task
                await asyncio.wait_for(asyncio.gather(*requests), 1)
            self.assertFalse(self.ledger.active)
        self.assertEqual(self.scope.finished, 2)

    async def test_model_native_id_reuse_has_distinct_source_identity(self):
        value = {
            "type": "message",
            "content": [
                {"type": "tool_use", "id": "same-native-id", "name": "tool", "input": {"value": 1}}
            ],
        }
        first = await rewrite_message(json.loads(json.dumps(value)), self.ledger)
        second = await rewrite_message(json.loads(json.dumps(value)), self.ledger)
        self.assertNotEqual(first["content"][0]["id"], second["content"][0]["id"])
        self.assertEqual(value["content"][0]["id"], "same-native-id")
        with self.assertRaises(CallbackFailure):
            await rewrite_message(value, self.ledger)

    async def test_stream_registration_precedes_tool_completion_and_rejects_reopen(self):
        stream = MessageStream(self.ledger)
        await stream.event({"type": "message_start", "message": {"content": []}})
        start = {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "name": "tool", "id": "native", "input": {}},
        }
        transformed = await stream.event(start)
        self.assertNotEqual(transformed["content_block"]["id"], "native")
        self.assertEqual(self.scope.registered, 0)
        await stream.event(
            {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": '{"value":1}'},
            }
        )
        await stream.event({"type": "content_block_stop", "index": 0})
        self.assertEqual(self.scope.registered, 1)
        with self.assertRaises(CallbackFailure):
            await stream.event(start)

    async def test_unknown_model_blocks_and_incomplete_stream_are_rejected(self):
        stream = MessageStream(self.ledger)
        with self.assertRaises(CallbackFailure):
            await stream.event({"type": "message_stop"})
        await stream.event({"type": "message_start", "message": {"content": []}})
        with self.assertRaises(CallbackFailure):
            await stream.event(
                {
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "server_tool_use"},
                }
            )


if __name__ == "__main__":
    unittest.main()
