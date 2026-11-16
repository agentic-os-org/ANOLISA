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
        except (ConnectionError, asyncio.IncompleteReadError):
            pass
        finally:
            writer.close()
            self.writers.discard(writer)
            self.operations.discard(task)

    async def asyncTearDown(self) -> None:
        self.server.close()
        await self.server.wait_closed()
        for task in list(self.operations):
            task.cancel()
        await asyncio.gather(*self.operations, return_exceptions=True)
        for writer in self.writers:
            writer.close()
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
