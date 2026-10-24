"""A private loopback Anthropic relay binds tool identities before native dispatch."""

from __future__ import annotations

import asyncio
import json
import secrets
import ssl
import sys
from typing import Any
from urllib.parse import urlsplit
from urllib.request import proxy_bypass_environment

import httpx

from aw_claude import CallbackFailure
from aw_model_stream import MAX_FRAME, MessageStream, rewrite_message

MAX_REQUEST = 32 * 1024 * 1024
HOP_HEADERS = {
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "content-encoding",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "upgrade",
}


class ModelRelay:
    """One bounded relay per client; no upstream redirects or direct fallback path."""

    def __init__(self, bridge: Any, environment: dict[str, str]) -> None:
        self.bridge = bridge
        self.upstream = environment.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com").rstrip(
            "/"
        )
        parsed = urlsplit(self.upstream)
        if (
            parsed.scheme not in {"http", "https"}
            or not parsed.hostname
            or parsed.username
            or parsed.password
            or parsed.query
            or parsed.fragment
        ):
            raise ValueError(
                "AW requires an HTTP(S) Anthropic base URL without embedded credentials"
            )
        if parsed.scheme == "http" and parsed.hostname not in {"localhost", "127.0.0.1", "::1"}:
            raise ValueError("AW requires TLS for non-loopback model endpoints")
        if any(
            environment.get(key) not in {None, "", "0"}
            for key in (
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            )
        ):
            raise ValueError(
                "AW managed identities currently require the Anthropic Messages transport"
            )
        self.secret = secrets.token_urlsafe(32)
        proxies = {
            key[:-6].lower(): value
            for key, value in environment.items()
            if key.lower().endswith("_proxy") and value
        }
        self.proxy = (
            None
            if proxy_bypass_environment(parsed.hostname, proxies)
            else (proxies.get(parsed.scheme) or proxies.get("all"))
        )
        if self.proxy and urlsplit(self.proxy).scheme not in {"http", "https"}:
            raise ValueError("AW model relay requires an HTTP(S) proxy or matching NO_PROXY")
        self.tls = ssl.create_default_context(
            cafile=environment.get("SSL_CERT_FILE"), capath=environment.get("SSL_CERT_DIR")
        )
        self.server: asyncio.Server | None = None
        self.tasks: set[asyncio.Task[Any]] = set()
        self.http: httpx.AsyncClient | None = None

    async def start(self) -> str:
        self.http = httpx.AsyncClient(
            timeout=httpx.Timeout(60, connect=10, pool=10),
            follow_redirects=False,
            trust_env=False,
            proxy=self.proxy,
            verify=self.tls,
            limits=httpx.Limits(max_connections=8, max_keepalive_connections=8),
        )
        self.server = await asyncio.start_server(self.accept, "127.0.0.1", 0, limit=65536)
        return f"http://127.0.0.1:{self.server.sockets[0].getsockname()[1]}/{self.secret}"

    async def accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.current_task()
        if len(self.tasks) >= 8:
            writer.close()
            await writer.wait_closed()
            return
        self.tasks.add(task)
        try:
            async with asyncio.timeout(600):
                await self.request(reader, writer)
        except asyncio.CancelledError:
            raise
        except Exception as error:
            print(
                f"AW managed model response failed: {type(error).__name__}",
                file=sys.stderr,
                flush=True,
            )
            # Partial responses cannot be turned into an error response. Close
            # the stream and latch the client before any later model request.
            self.bridge.failure_reply("tool.after", {})
            await self.bridge.stop_task()
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except (ConnectionError, OSError):
                pass
            self.tasks.discard(task)

    async def request(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 10)
        lines = header.decode("latin-1").split("\r\n")
        method, target, protocol = lines[0].split(" ")
        if method != "POST" or protocol != "HTTP/1.1" or not target.startswith(f"/{self.secret}/"):
            writer.write(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            await writer.drain()
            return
        path = target[len(self.secret) + 1 :]
        if urlsplit(path).path not in {"/v1/messages", "/v1/messages/count_tokens"}:
            raise CallbackFailure("AW model endpoint is unsupported")
        headers: dict[str, str] = {}
        for line in lines[1:]:
            if not line:
                continue
            key, value = line.split(":", 1)
            key = key.strip().lower()
            if key in headers:
                raise CallbackFailure("AW model request has duplicate headers")
            headers[key] = value.strip()
        length = headers.get("content-length", "")
        if (
            not length.isascii()
            or not length.isdecimal()
            or not 0 < int(length) <= MAX_REQUEST
            or "transfer-encoding" in headers
            or "content-encoding" in headers
        ):
            raise CallbackFailure("AW requires a bounded uncompressed model request")
        data = await asyncio.wait_for(reader.readexactly(int(length)), 30)
        body = json.loads(data)
        await self.bridge.invocations.check_model_request(body)
        forwarded = {key: value for key, value in headers.items() if key not in HOP_HEADERS}
        forwarded["accept-encoding"] = "identity"
        async with self.http.stream(
            "POST", self.upstream + path, headers=forwarded, content=data
        ) as response:
            if 300 <= response.status_code < 400:
                raise CallbackFailure("AW model redirects are unsupported")
            out = {key: value for key, value in response.headers.items() if key not in HOP_HEADERS}
            out["connection"] = "close"
            writer.write(f"HTTP/1.1 {response.status_code} {response.reason_phrase}\r\n".encode())
            for key, value in out.items():
                writer.write(f"{key}: {value}\r\n".encode("latin-1"))
            writer.write(b"\r\n")
            if response.status_code != 200 or urlsplit(path).path.endswith("count_tokens"):
                size = 0
                async for chunk in response.aiter_bytes():
                    size += len(chunk)
                    if size > MAX_FRAME:
                        raise CallbackFailure("AW model auxiliary response exceeds one MiB")
                    writer.write(chunk)
                    await writer.drain()
            elif response.headers.get("content-type", "").split(";")[0] == "text/event-stream":
                await self.stream(response, writer)
            else:
                data = bytearray()
                async for chunk in response.aiter_bytes():
                    data.extend(chunk)
                    if len(data) > MAX_REQUEST:
                        raise CallbackFailure("AW model response exceeds 32 MiB")
                value = await rewrite_message(json.loads(data), self.bridge.invocations)
                writer.write(json.dumps(value, ensure_ascii=False).encode())
                await writer.drain()

    async def stream(self, response: httpx.Response, writer: asyncio.StreamWriter) -> None:
        stream = MessageStream(self.bridge.invocations)
        pending = bytearray()
        async for chunk in response.aiter_bytes():
            pending.extend(chunk)
            while True:
                separators = [
                    (pending.find(separator), len(separator))
                    for separator in (b"\n\n", b"\r\n\r\n")
                    if separator in pending
                ]
                if not separators:
                    break
                position, length = min(separators)
                frame = pending[:position]
                pending = pending[position + length :]
                if len(frame) > MAX_FRAME:
                    raise CallbackFailure("AW model SSE frame exceeds one MiB")
                data = b"\n".join(
                    line[5:].lstrip(b" ")
                    for line in frame.splitlines()
                    if line.startswith(b"data:")
                )
                if data:
                    value = await stream.event(json.loads(data))
                    frame = (
                        b"event: "
                        + value["type"].encode()
                        + b"\ndata: "
                        + json.dumps(value, ensure_ascii=False).encode()
                    )
                writer.write(frame + b"\n\n")
                await writer.drain()
            if len(pending) > MAX_FRAME:
                raise CallbackFailure("AW model SSE frame exceeds one MiB")
        if pending.strip() or not stream.stopped:
            raise CallbackFailure("AW model stream ended before message_stop")

    async def close(self) -> None:
        if self.server:
            self.server.close()
            await self.server.wait_closed()
        pending = list(self.tasks)
        for task in pending:
            task.cancel()
        async with asyncio.timeout(5):
            await asyncio.gather(*pending, return_exceptions=True)
            if self.http:
                await self.http.aclose()
