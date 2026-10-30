"""Exercise the real HTTP relay with bounded loopback peers and no model secrets."""

import asyncio
import json
import os
import unittest
from unittest.mock import patch

from identity_contract import Scope
from types import SimpleNamespace
from aw_invocations import Invocations
import aw_model
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
        self.pause = False
        self.waiting, self.release = asyncio.Event(), asyncio.Event()

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
                        f"HTTP/1.1 {self.status} Test\r\nContent-Type: {self.content_type}\r\nContent-Length: {len(self.body) + int(self.pause)}\r\nConnection: close\r\n\r\n".encode()
                        + self.body
                    )
                    await writer.drain()
                    if self.pause:
                        self.waiting.set()
                        await self.release.wait()
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
        self.release.set()
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

    async def test_unauthenticated_disconnect_and_bad_header_leave_session_usable(self):
        for data in (b"", b"garbage\r\n\r\n"):
            reader, writer = await asyncio.open_connection("127.0.0.1", self.ports[1])
            writer.write(data)
            await writer.drain()
            writer.close()
            await writer.wait_closed()
        async with asyncio.timeout(1):
            while self.relay.tasks:
                await asyncio.sleep(0.001)
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.requests)
        self.assertEqual((await self.post()).status_code, 200)

    async def test_stream_disconnect_drains_upstream_and_next_execution_reuses_relay(self):
        original = self.body
        self.pause, self.content_type = True, "text/event-stream"
        self.body = b'data: {"type":"message_start","message":{"content":[]}}\n\n'
        async with self.http.stream("POST", self.endpoint + "/v1/messages",
                                    content=b'{"messages":[]}') as response:
            async for chunk in response.aiter_bytes():
                self.assertIn(b"message_start", chunk)
                break
        await asyncio.wait_for(self.waiting.wait(), 1)
        await asyncio.wait_for(self.relay.drain(), 1)
        self.assertIsNone(self.bridge.failure)
        self.assertFalse(self.stopped.is_set())
        self.assertFalse(self.relay.exchanges)
        await self.bridge.invocations.drain()
        self.bridge.invocations.begin()
        self.release.set()
        self.pause, self.content_type, self.body = False, "application/json", original
        self.assertEqual((await self.post()).status_code, 200)
        self.assertEqual(len(self.requests), 2)

    async def connected(self, count):
        peers = [await asyncio.open_connection("127.0.0.1", self.ports[1])
                 for _ in range(count)]
        async with asyncio.timeout(1):
            while len(self.relay.tasks) < count:
                await asyncio.sleep(0)
        return peers

    async def close_peers(self, peers):
        for _, writer in peers:
            writer.close()
        await asyncio.gather(*(writer.wait_closed() for _, writer in peers),
                             return_exceptions=True)

    async def test_close_cancels_idle_connections_before_waiting_for_server(self):
        peers = await self.connected(1)
        try:
            await asyncio.wait_for(self.relay.close(), 1)
            self.assertEqual(await asyncio.wait_for(peers[0][0].read(), 1), b"")
            self.assertFalse(self.relay.tasks)
            self.assertTrue(self.relay.http.is_closed)
        finally:
            await self.close_peers(peers)

    async def test_close_cancels_stalled_authenticated_exchange(self):
        self.pause, self.content_type = True, "text/event-stream"
        self.body = b'data: {"type":"message_start","message":{"content":[]}}\n\n'
        request = asyncio.create_task(self.post())
        try:
            await asyncio.wait_for(self.waiting.wait(), 1)
            await asyncio.wait_for(self.relay.close(), 1)
            self.assertFalse(self.relay.tasks)
            self.assertFalse(self.relay.exchanges)
            self.assertTrue(self.relay.http.is_closed)
            self.assertIsNone(self.bridge.failure)
        finally:
            self.release.set()
            await asyncio.gather(request, return_exceptions=True)

    async def test_idle_peers_do_not_consume_authenticated_exchange_slots(self):
        peers = await self.connected(8)
        try:
            self.assertEqual((await self.post()).status_code, 200)
            self.assertEqual(len(self.requests), 1)
            self.assertIsNone(self.bridge.failure)
        finally:
            await self.close_peers(peers)

    async def test_handshake_capacity_and_deadline_are_bounded(self):
        peers = await self.connected(aw_model.MAX_HANDSHAKES)
        extra = []
        try:
            extra = [await asyncio.open_connection("127.0.0.1", self.ports[1])]
            self.assertEqual(await asyncio.wait_for(extra[0][0].read(), 1), b"")
            self.assertLessEqual(len(self.relay.handshakes), aw_model.MAX_HANDSHAKES)
            await asyncio.wait_for(asyncio.gather(*(reader.read() for reader, _ in peers)), 2)
            self.assertFalse(self.requests)
            self.assertIsNone(self.bridge.failure)
            self.assertEqual((await self.post()).status_code, 200)
        finally:
            await self.close_peers(peers + extra)

    async def test_authenticated_exchange_overload_remains_fail_closed(self):
        peers = await self.connected(aw_model.MAX_EXCHANGES)
        try:
            for _, writer in peers:
                writer.write((f"POST /{self.relay.secret}/v1/messages HTTP/1.1\r\n"
                             "Content-Length: 2\r\n\r\n").encode())
                await writer.drain()
            async with asyncio.timeout(1):
                while len(self.relay.exchanges) < aw_model.MAX_EXCHANGES:
                    await asyncio.sleep(0)
            with self.assertRaises(httpx.RemoteProtocolError):
                await self.post()
            await asyncio.wait_for(self.stopped.wait(), 1)
            self.assertIsNotNone(self.bridge.failure)
            self.assertFalse(self.requests)
        finally:
            await self.close_peers(peers)

    async def test_close_deadline_keeps_unjoined_cleanup_visible(self):
        entered, cleanup, release = asyncio.Event(), asyncio.Event(), asyncio.Event()

        async def producer():
            try:
                entered.set()
                await asyncio.Event().wait()
            finally:
                cleanup.set()
                await release.wait()

        task = asyncio.create_task(producer())
        self.relay.tasks.add(task)
        await entered.wait()
        try:
            with patch.object(aw_model, "CLOSE_TIMEOUT", 0.05):
                with self.assertRaises(TimeoutError):
                    await self.relay.close()
            self.assertTrue(cleanup.is_set())
            self.assertFalse(task.done())
            self.assertIn(task, self.relay.tasks)
        finally:
            release.set()
            await asyncio.gather(task, return_exceptions=True)
            self.relay.tasks.discard(task)

    async def test_upstream_disconnect_remains_fail_closed(self):
        self.upstream.close()
        await self.upstream.wait_closed()
        with self.assertRaises(httpx.RemoteProtocolError):
            await self.post()
        await asyncio.wait_for(self.stopped.wait(), 1)
        self.assertIsNotNone(self.bridge.failure)

    async def proxy_route(self, bypass):
        await self.relay.close()
        # A closed, OS-assigned port makes a mistaken proxy route observable.
        proxy = await asyncio.start_server(lambda *args: None, "127.0.0.1", 0)
        port = proxy.sockets[0].getsockname()[1]
        proxy.close()
        await proxy.wait_closed()
        self.ports.append(port)
        self.relay = ModelRelay(self.bridge, {
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{self.ports[0]}",
            "HTTP_PROXY": f"http://127.0.0.1:{port}",
            "NO_PROXY": bypass,
        })
        self.endpoint = await self.relay.start()
        self.ports.append(self.relay.server.sockets[0].getsockname()[1])
        print(json.dumps({"pid": os.getpid(), "ports": self.ports}))

    async def test_no_proxy_matching_host_port_reaches_upstream(self):
        await self.proxy_route(f"127.0.0.1:{self.ports[0]}")
        response = await self.post()
        self.assertIsNone(self.relay.proxy)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(len(self.requests), 1)
        self.assertIsNone(self.bridge.failure)

    async def test_no_proxy_other_port_keeps_proxy_failure_closed(self):
        await self.proxy_route(f"127.0.0.1:{self.ports[0] % 65535 + 1}")
        self.assertIsNotNone(self.relay.proxy)
        with self.assertRaises(httpx.RemoteProtocolError):
            await self.post()
        await asyncio.wait_for(self.stopped.wait(), 1)
        self.assertFalse(self.requests)
        self.assertIsNotNone(self.bridge.failure)

    async def test_no_proxy_host_without_port_still_reaches_upstream(self):
        await self.proxy_route("127.0.0.1")
        self.assertIsNone(self.relay.proxy)
        self.assertEqual((await self.post()).status_code, 200)
        self.assertEqual(len(self.requests), 1)

    async def test_no_proxy_authority_preserves_ipv6_brackets_and_exact_port(self):
        for host in ("api.example.test", "[::1]"):
            for bypass, direct in ((host, True), (host.strip("[]"), True), (f"{host}:8443", True),
                                   (f"{host}:8444", False)):
                with self.subTest(host=host, bypass=bypass):
                    relay = ModelRelay(self.bridge, {
                        "ANTHROPIC_BASE_URL": f"https://{host}:8443/prefix",
                        "HTTPS_PROXY": "http://127.0.0.1:1", "NO_PROXY": bypass,
                    })
                    self.assertEqual(relay.proxy is None, direct)

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
