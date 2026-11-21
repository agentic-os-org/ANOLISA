"""Wire failures, registration recovery and cancellation share the SDK fixture."""

import asyncio
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest

from aw_sdk import AwError, Instance
from aw_sdk.transport import rpc

CONTRACT = json.loads(Path(os.environ["AW_CONTRACT_FIXTURE"]).read_text())
IDENTITY = {"generation": "test-generation", "config_revision": "test-revision"}


class Transport(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="aw-sdk-wire-")
        self.endpoint = str(Path(self.directory.name) / "aw.sock")
        self.requests = []
        self.writers = set()
        self.operations = set()
        self.after_reply = None
        self.handler = None
        self.server = await asyncio.start_unix_server(self.accept, self.endpoint)
        Path(self.endpoint).chmod(0o600)

    async def accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.current_task()
        self.operations.add(task)
        self.writers.add(writer)
        try:
            (length,) = struct.unpack("!I", await reader.readexactly(4))
            operation = json.loads(await reader.readexactly(length))["operation"]
            self.requests.append(operation)
            result, error = await self.handler(operation)
            body = json.dumps(
                {
                    "api_version": CONTRACT["service_protocol"],
                    "identity": IDENTITY,
                    "result": result,
                    "error": error,
                }
            ).encode()
            writer.write(struct.pack("!I", len(body)) + body)
            await writer.drain()
            if self.after_reply is not None:
                await self.after_reply(reader, writer)
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            self.writers.discard(writer)
            self.operations.discard(task)

    async def asyncTearDown(self) -> None:
        self.server.close()
        for task in list(self.operations):
            task.cancel()
        await asyncio.gather(*self.operations, return_exceptions=True)
        for writer in self.writers:
            writer.close()
        await self.server.wait_closed()
        self.directory.cleanup()

    async def test_remote_errors_never_become_transport_loss(self) -> None:
        for code in [*CONTRACT["never_fallback"], "ECONNRESET"]:

            async def error(_: dict) -> tuple:
                return None, code

            self.handler = error
            with self.assertRaises(AwError) as raised:
                await rpc(self.endpoint, IDENTITY, {"method": "status"}, 1000)
            self.assertEqual(raised.exception.code, code)
            self.assertFalse(raised.exception.unavailable)

    async def test_validated_reply_waits_for_slot_retirement(self) -> None:
        retired = 0
        replies = iter([({"ok": True}, None), (None, "denied")])

        async def handler(_: dict) -> tuple:
            return next(replies)

        async def retire(reader: asyncio.StreamReader, _: asyncio.StreamWriter) -> None:
            nonlocal retired
            if await reader.read(1) == b"":
                retired += 1

        self.handler = handler
        self.after_reply = retire
        operation = {"method": "status"}
        response = await rpc(self.endpoint, IDENTITY, operation, 1000)
        self.assertEqual(response["result"], {"ok": True})
        self.assertEqual(retired, 1)
        with self.assertRaises(AwError) as raised:
            await rpc(self.endpoint, IDENTITY, operation, 1000)
        self.assertEqual(raised.exception.code, "denied")
        self.assertFalse(raised.exception.unavailable)
        self.assertEqual(retired, 2)

    async def test_stalled_peer_preserves_reply_and_caller_cancellation(self) -> None:
        half_closed = asyncio.Event()

        async def stall(reader: asyncio.StreamReader, _: asyncio.StreamWriter) -> None:
            if await reader.read(1) == b"":
                half_closed.set()
            await asyncio.Event().wait()

        async def handler(_: dict) -> tuple:
            return {"ok": True}, None

        self.handler = handler
        self.after_reply = stall
        operation = {"method": "status"}
        response = await asyncio.wait_for(rpc(self.endpoint, IDENTITY, operation, 150), 2)
        self.assertEqual(response["result"], {"ok": True})
        self.assertTrue(half_closed.is_set())
        half_closed.clear()
        pending = asyncio.create_task(rpc(self.endpoint, IDENTITY, operation, 1000))
        await asyncio.wait_for(half_closed.wait(), 1)
        pending.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await pending
        self.assertEqual(len(self.requests), 2)

    async def test_trailing_reply_bytes_are_invalid(self) -> None:
        async def handler(_: dict) -> tuple:
            return {"ok": True}, None

        async def trailing(_: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            writer.write(b"x")
            await writer.drain()

        self.handler = handler
        self.after_reply = trailing
        with self.assertRaises(AwError) as raised:
            await rpc(self.endpoint, IDENTITY, {"method": "status"}, 1000)
        self.assertEqual(raised.exception.code, "invalid_frame")

    async def test_registration_response_loss_uses_query(self) -> None:
        async def handler(operation: dict) -> tuple:
            if operation["method"] == "status":
                return {"sdk_protocol": CONTRACT["sdk_protocol"]}, None
            action = operation["command"]["action"]
            if action == "register_sdk":
                raise ConnectionResetError()
            if action == "query":
                return {
                    "state": "pending_ready",
                    "handle": {"binding": {"instance_id": "instance"}},
                    "policy_revision": "revision",
                    "routes": {"events": {}, "hooks": []},
                }, None
            return {"state": "ready" if action == "ready" else "released"}, None

        self.handler = handler
        instance = Instance(
            endpoint=self.endpoint,
            target="app",
            capabilities={},
            availability="native_fallback",
            install=lambda _: True,
        )
        try:
            self.assertFalse(await instance.connect())
            self.assertTrue(await instance.connect())
            actions = [
                row["command"]["action"] for row in self.requests if row["method"] == "registration"
            ]
            self.assertEqual(actions, ["register_sdk", "query", "ready"])
        finally:
            await instance.close()

    async def test_timeout_late_reply_and_user_cancellation_do_not_replay(self) -> None:
        async def handler(_: dict) -> tuple:
            await asyncio.sleep(0.1)
            return {"late": True}, None

        self.handler = handler
        with self.assertRaises(AwError) as raised:
            await rpc(self.endpoint, IDENTITY, {"method": "status"}, 20)
        self.assertTrue(raised.exception.unavailable)
        pending = asyncio.create_task(rpc(self.endpoint, IDENTITY, {"method": "status"}, 1000))
        await asyncio.sleep(0.02)
        pending.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await pending
        await asyncio.sleep(0.12)
        self.assertEqual(len(self.requests), 2)
