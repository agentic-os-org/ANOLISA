"""Exercise the real HTTP relay with bounded loopback peers and no model secrets."""

import asyncio
import json
import os
import unittest

from identity_contract import Scope
from types import SimpleNamespace
from aw_invocations import Invocations
from aw_model import ModelRelay, MAX_REQUEST
import httpx


class RelayTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.bridge = SimpleNamespace(scope=Scope(), failure=None)
        self.bridge.invocations = Invocations(self.bridge)
        self.bridge.invocations.begin()
        self.stopped = asyncio.Event()
        self.bridge.failure_reply = lambda *args: setattr(self.bridge, "failure", "relay_failure")

        async def stop():
            self.stopped.set()

        self.bridge.stop_task = stop
        self.requests = []
        self.workers = set()
        self.body = json.dumps(
            {
                "type": "message",
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Bash",
                        "id": "reused-native-id",
                        "input": {"command": "true"},
                    }
                ],
            }
        ).encode()
        self.status, self.content_type = 200, "application/json"

        async def upstream(reader, writer):
            task = asyncio.current_task()
            self.workers.add(task)
            try:
                async with asyncio.timeout(3):
                    header = await reader.readuntil(b"\r\n\r\n")
                    fields = dict(line.split(b":", 1) for line in header.split(b"\r\n")[1:] if line)
                    data = await reader.readexactly(int(fields[b"Content-Length"]))
                    self.requests.append((header, data))
                    writer.write(
                        f"HTTP/1.1 {self.status} Test\r\nContent-Type: {self.content_type}\r\nContent-Length: {len(self.body)}\r\nConnection: close\r\n\r\n".encode()
                        + self.body
                    )
                    await writer.drain()
            finally:
                writer.close()
                await writer.wait_closed()
                self.workers.discard(task)

        self.upstream = await asyncio.start_server(upstream, "127.0.0.1", 0)
        port = self.upstream.sockets[0].getsockname()[1]
        self.relay = ModelRelay(self.bridge, {"ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}"})
        self.endpoint = await self.relay.start()
        self.http = httpx.AsyncClient(trust_env=False, timeout=3)
        self.addAsyncCleanup(self.cleanup)
        self.ports = [port, self.relay.server.sockets[0].getsockname()[1]]
        print(
            json.dumps(
                {
                    "pid": os.getpid(),
                    "ports": self.ports,
                    "lifetime_seconds": 60,
                    "stop": "RelayTests.cleanup closes listeners and joins request tasks",
                }
            )
        )

    async def cleanup(self):
        await self.http.aclose()
        await self.relay.close()
        self.upstream.close()
        await self.upstream.wait_closed()
        async with asyncio.timeout(5):
            await asyncio.gather(*self.workers)
        self.assertFalse(self.relay.tasks)
        for port in self.ports:
            with self.assertRaises(OSError):
                await asyncio.wait_for(asyncio.open_connection("127.0.0.1", port), 1)

    async def post(self):
        return await self.http.post(
            self.endpoint + "/v1/messages",
            content=b'{"messages":[]}',
            headers={"x-api-key": "fixture-only", "content-type": "application/json"},
        )

    async def test_json_identity_is_registered_before_response_and_request_is_preserved(self):
        response = await self.post()
        call = response.json()["content"][0]["id"]
        self.assertNotEqual(call, "reused-native-id")
        self.assertEqual(self.bridge.invocations.active[call].phase, "issued")
        self.assertEqual(self.requests[0][1], b'{"messages":[]}')
        self.assertIn(b"x-api-key: fixture-only", self.requests[0][0])
        self.assertFalse(self.stopped.is_set())

    async def test_unauthorized_path_cannot_reach_upstream(self):
        response = await self.http.post(
            self.endpoint.rsplit("/", 1)[0] + "/wrong/v1/messages", content=b"{}"
        )
        self.assertEqual(response.status_code, 403)
        self.assertFalse(self.requests)
        self.assertFalse(self.stopped.is_set())

    async def test_redirect_latches_failure_without_forwarding_to_another_origin(self):
        self.status = 307
        with self.assertRaises(httpx.RemoteProtocolError):
            await self.post()
        await asyncio.wait_for(self.stopped.wait(), 1)
        self.assertEqual(len(self.requests), 1)
        self.assertFalse(self.bridge.invocations.active)

    async def test_partial_sse_never_registers_an_incomplete_tool(self):
        self.content_type = "text/event-stream"
        self.body = b'data: {"type":"message_start","message":{"content":[]}}\n\n'
        await self.post()
        await asyncio.wait_for(self.stopped.wait(), 1)
        self.assertFalse(self.bridge.invocations.active)

    async def test_upstream_http_error_preserves_native_error_response(self):
        self.status, self.body = 429, b'{"type":"error"}'
        response = await self.post()
        self.assertEqual(response.status_code, 429)
        self.assertEqual(response.content, self.body)
        self.assertFalse(self.bridge.invocations.active)
        self.assertFalse(self.stopped.is_set())

    async def test_request_overload_stops_before_upstream(self):
        host = self.endpoint.split("/")[2]
        secret = self.endpoint.rsplit("/", 1)[1]
        reader, writer = await asyncio.open_connection("127.0.0.1", self.ports[1])
        try:
            writer.write(
                f"POST /{secret}/v1/messages HTTP/1.1\r\nHost: {host}\r\nContent-Length: {MAX_REQUEST + 1}\r\n\r\n".encode()
            )
            await writer.drain()
            self.assertEqual(await asyncio.wait_for(reader.read(), 2), b"")
            await asyncio.wait_for(self.stopped.wait(), 1)
            self.assertFalse(self.requests)
        finally:
            writer.close()
            await writer.wait_closed()

    async def test_unsupported_model_transport_fails_at_factory_boundary(self):
        for environment in (
            {"ANTHROPIC_BASE_URL": "http://example.com"},
            {"CLAUDE_CODE_USE_BEDROCK": "1"},
            {"HTTPS_PROXY": "socks5://127.0.0.1:1"},
        ):
            with self.assertRaises(ValueError):
                ModelRelay(self.bridge, environment)


if __name__ == "__main__":
    unittest.main()
