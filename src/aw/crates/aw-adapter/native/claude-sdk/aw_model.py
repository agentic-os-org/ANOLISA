"""A private loopback Anthropic relay binds tool identities before native dispatch."""

from __future__ import annotations

import asyncio
import json
import re
import secrets
import ssl
from typing import Any
from urllib.parse import urlsplit
from urllib.request import proxy_bypass_environment

import httpx

if __package__:
    from aw_sdk.aw_http_hosts import allows_http
    from .aw_claude import CallbackFailure
    from .aw_model_stream import (
        MAX_FRAME, MessageStream, event_name, json_error_label, relay_log, rewrite_message,
        sse_fields, sse_frames,
    )
else:
    from aw_http_hosts import allows_http
    from aw_claude import CallbackFailure
    from aw_model_stream import (
        MAX_FRAME, MessageStream, event_name, json_error_label, relay_log, rewrite_message,
        sse_fields, sse_frames,
    )

MAX_REQUEST = 32 * 1024 * 1024
MAX_HANDSHAKES = 32
MAX_EXCHANGES = 8
HEADER_TIMEOUT = 1
CLOSE_TIMEOUT = 5
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


def media_label(media_type: str) -> str:
    # Even a syntactically valid vendor subtype can contain private text.
    if media_type in {"text/event-stream", "application/json", "application/problem+json",
                      "application/octet-stream", "text/plain", "text/html"}:
        return media_type
    token = r"[a-z0-9!#$%&'*+.^_`|~-]+"
    if len(media_type) <= 127 and re.fullmatch(f"{token}/{token}", media_type):
        return "other"
    return "invalid"


class DownstreamClosed(Exception):
    """The native client stopped consuming its request or response."""


async def send(writer: asyncio.StreamWriter, data: bytes) -> None:
    try:
        writer.write(data)
        await writer.drain()
    except (ConnectionError, OSError) as error:
        raise DownstreamClosed from error


def _normalize_no_proxy_port(rule: str) -> str:
    host, _, port = rule.strip().rpartition(":")
    # A bare IPv6 literal has no port, even when its last hextet is decimal.
    if not host or (":" in host and not host.endswith("]")):
        return rule
    if port.isascii() and port.isdecimal():
        return f"{host}:{port.lstrip('0') or '0'}"
    return rule


class ModelRelay:
    """One bounded relay per client; no upstream redirects or direct fallback path."""

    def __init__(
        self, bridge: Any, environment: dict[str, str], *,
        allow_plaintext_http_hosts: list[str] | None = None,
    ) -> None:
        self.bridge = bridge
        self.upstream = environment.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com").rstrip(
            "/"
        )
        parsed = urlsplit(self.upstream)
        if (
            parsed.scheme not in {"http", "https"}
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or "?" in self.upstream
            or "#" in self.upstream
        ):
            raise ValueError(
                "AW requires an HTTP(S) Anthropic base URL without embedded credentials"
            )
        granted_http = parsed.scheme == "http" and parsed.hostname not in {"localhost", "127.0.0.1", "::1"}
        if granted_http and not allows_http(parsed.netloc, allow_plaintext_http_hosts or []):
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
        host = parsed.hostname
        port = parsed.port if parsed.port is not None else (443 if parsed.scheme == "https" else 80)
        authority = f"[{host}]:{port}" if ":" in host else f"{host}:{port}"
        if "no" in proxies:
            proxies["no"] = ",".join(_normalize_no_proxy_port(rule) for rule in proxies["no"].split(","))
        bypass = proxy_bypass_environment(authority, proxies)
        # urllib can split the last IPv6 hextet as a port. Match bare literals whole.
        if ":" in host and not bypass:
            bypass = any(host == rule.strip().lower() for rule in proxies.get("no", "").split(","))
        self.proxy = None if bypass else (proxies.get(parsed.scheme) or proxies.get("all"))
        if self.proxy and urlsplit(self.proxy).scheme not in {"http", "https"}:
            raise ValueError("AW model relay requires an HTTP(S) proxy or matching NO_PROXY")
        # HTTP Core 1.0 drops IPv6 brackets when building a forward proxy's
        # absolute-form request target. Preserve the checked authority on the wire.
        self._ipv6_forward_proxy = self.proxy is not None and parsed.scheme == "http" and ":" in host
        self.tls = ssl.create_default_context(
            cafile=environment.get("SSL_CERT_FILE"), capath=environment.get("SSL_CERT_DIR")
        )
        self.server: asyncio.Server | None = None
        self.tasks: set[asyncio.Task[Any]] = set()
        self.handshakes: set[asyncio.Task[Any]] = set()
        self.exchanges: set[asyncio.Task[Any]] = set()
        self.http: httpx.AsyncClient | None = None
        self.closing = False

    async def start(self) -> str:
        self.http = httpx.AsyncClient(
            timeout=httpx.Timeout(60, connect=10, pool=10),
            follow_redirects=False,
            trust_env=False,
            proxy=self.proxy,
            verify=self.tls,
            limits=httpx.Limits(max_connections=MAX_EXCHANGES, max_keepalive_connections=8),
        )
        self.server = await asyncio.start_server(self.accept, "127.0.0.1", 0, limit=65536)
        return f"http://127.0.0.1:{self.server.sockets[0].getsockname()[1]}/{self.secret}"

    async def accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        task = asyncio.current_task()
        self.tasks.add(task)
        details = {"stage": "request_http"}
        try:
            if self.closing or len(self.handshakes) >= MAX_HANDSHAKES:
                return
            self.handshakes.add(task)
            async with asyncio.timeout(600):
                await self.request(reader, writer, details)
        except asyncio.CancelledError:
            raise
        except DownstreamClosed:
            # interrupt() closes the CLI's HTTP stream. Upstream or protocol
            # failures still propagate separately and poison the task below.
            pass
        except Exception as error:
            if isinstance(error, json.JSONDecodeError) and details["stage"] in {
                "request_json", "response_json", "sse_frame", "tool_input_json"
            }:
                details.update(json_err=json_error_label(error), json_pos=error.pos)
            relay_log(f"failed: {type(error).__name__}", details)
            # Partial responses cannot be turned into an error response. Close
            # the stream and latch the client before any later model request.
            self.bridge.failure_reply("tool.after", {})
            await self.bridge.stop_task()
        finally:
            self.handshakes.discard(task)
            writer.close()
            try:
                await writer.wait_closed()
            except (ConnectionError, OSError):
                pass
            finally:
                self.tasks.discard(task)

    async def request(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter, details: dict,
    ) -> None:
        try:
            header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), HEADER_TIMEOUT)
            lines = header.decode("latin-1").split("\r\n")
            method, target, protocol = lines[0].split(" ")
        except (asyncio.IncompleteReadError, asyncio.LimitOverrunError,
                TimeoutError, ConnectionError, ValueError):
            # No authenticated session operation exists at this boundary.
            return
        if method != "POST" or protocol != "HTTP/1.1" or not target.startswith(f"/{self.secret}/"):
            await send(writer,
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            return
        task = asyncio.current_task()
        self.handshakes.discard(task)
        if len(self.exchanges) >= MAX_EXCHANGES:
            raise CallbackFailure("AW active model request limit exceeded")
        self.exchanges.add(task)
        try:
            await self.authenticated(reader, writer, target, lines, details)
        finally:
            self.exchanges.discard(task)

    async def authenticated(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
        target: str, lines: list[str], details: dict,
    ) -> None:
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
        try:
            data = await asyncio.wait_for(reader.readexactly(int(length)), 30)
        except (asyncio.IncompleteReadError, ConnectionError) as error:
            raise DownstreamClosed from error

        async def disconnected() -> None:
            try:
                extra = await reader.read(1)
            except ConnectionError:
                return
            if extra:
                raise CallbackFailure("AW model connection does not support pipelining")

        forwarding = asyncio.create_task(self.forward(path, headers, data, writer, details))
        closed = asyncio.create_task(disconnected())
        try:
            done, _ = await asyncio.wait((forwarding, closed), return_when=asyncio.FIRST_COMPLETED)
            if forwarding in done:
                await forwarding
            else:
                await closed
        finally:
            # Closing the downstream cancels a blocked upstream read. Join the
            # producer before Result can drain identities or begin another query.
            for task in (forwarding, closed):
                if not task.done():
                    task.cancel()
            results = await asyncio.gather(forwarding, closed, return_exceptions=True)
            for result in results:
                if isinstance(result, Exception) and not isinstance(result, DownstreamClosed):
                    raise result

    async def forward(
        self, path: str, headers: dict[str, str], data: bytes, writer: asyncio.StreamWriter, details: dict,
    ) -> None:
        details["stage"] = "request_json"
        body = json.loads(data)
        details["stage"] = "request_check"
        await self.bridge.invocations.check_model_request(body)
        forwarded = {key: value for key, value in headers.items() if key not in HOP_HEADERS}
        forwarded["accept-encoding"] = "identity"
        url = self.upstream + path
        extensions = {"target": str(httpx.URL(url)).encode("ascii")} if self._ipv6_forward_proxy else None
        details["stage"] = "upstream_http"
        async with self.http.stream(
            "POST", url, headers=forwarded, content=data, extensions=extensions
        ) as response:
            media_type = response.headers.get("content-type", "").split(";", 1)[0].strip().lower()
            details.update(status=response.status_code, content_type=media_label(media_type))
            if 300 <= response.status_code < 400:
                raise CallbackFailure("AW model redirects are unsupported")
            out = {key: value for key, value in response.headers.items() if key not in HOP_HEADERS}
            out["connection"] = "close"
            await send(writer, (
                f"HTTP/1.1 {response.status_code} {response.reason_phrase}\r\n"
                + "".join(f"{key}: {value}\r\n" for key, value in out.items()) + "\r\n"
            ).encode("latin-1"))
            if response.status_code != 200 or urlsplit(path).path.endswith("count_tokens"):
                size = 0
                async for chunk in response.aiter_bytes():
                    size += len(chunk)
                    if size > MAX_FRAME:
                        raise CallbackFailure("AW model auxiliary response exceeds one MiB")
                    await send(writer, chunk)
            elif media_type == "text/event-stream":
                await self.stream(response, writer, details)
            else:
                data = bytearray()
                async for chunk in response.aiter_bytes():
                    data.extend(chunk)
                    if len(data) > MAX_REQUEST:
                        raise CallbackFailure("AW model response exceeds 32 MiB")
                details["stage"] = "response_json"
                value = json.loads(data)
                details["stage"] = "response_message"
                value = await rewrite_message(value, self.bridge.invocations)
                await send(writer, json.dumps(value, ensure_ascii=False).encode())

    async def stream(
        self, response: httpx.Response, writer: asyncio.StreamWriter, details: dict,
    ) -> None:
        stream = MessageStream(self.bridge.invocations, details)
        http_details = details.copy()
        first = True
        async for frame, complete in sse_frames(response.aiter_bytes(), details):
            data, count, event = sse_fields(frame.removeprefix(b"\xef\xbb\xbf") if first else frame)
            first = False
            if not count:
                continue
            if not data.strip(b" \t\r\n"):
                stream.warn_once("blank_data", data_bytes=len(data), data_lines=count)
                continue
            if stream.stopped and data == b"[DONE]":
                stream.warn_once("trailing_done", frame_bytes=len(frame))
                continue
            details.update(stage="sse_frame", sse_event=event, frame_bytes=len(frame),
                           data_bytes=len(data), data_lines=count, blocks_open=len(stream.blocks),
                           stream_started=int(stream.started), stream_stopped=int(stream.stopped))
            try:
                value = json.loads(data.decode("utf-8"))
            except ValueError:
                details["payload_kind"] = "done" if data == b"[DONE]" else "non_json"
                raise
            if not complete:
                raise CallbackFailure("AW model stream ended inside an event")
            details.update(stage="stream_event",
                           sse_event=event_name(value.get("type") if isinstance(value, dict) else None))
            for outgoing in await stream.event(value):
                await send(writer, b"event: " + outgoing["type"].encode() + b"\ndata: "
                           + json.dumps(outgoing, ensure_ascii=False).encode() + b"\n\n")
            details.clear()
            details.update(http_details)
        if not stream.stopped:
            details.update(stage="stream_event", blocks_open=len(stream.blocks),
                           stream_started=int(stream.started), stream_stopped=0)
            raise CallbackFailure("AW model stream ended before message_stop")

    async def drain(self) -> None:
        """Join authenticated producers before releasing execution identities."""
        async with asyncio.timeout(5):
            await asyncio.gather(*(asyncio.shield(task) for task in self.exchanges))

    async def close(self) -> None:
        self.closing = True
        if self.server:
            self.server.close()
        pending = list(self.tasks)
        for task in pending:
            task.cancel()
        async with asyncio.timeout(CLOSE_TIMEOUT):
            # Python 3.12 waits for active transports in server.wait_closed().
            # Join handlers first; shield their cleanup so the deadline cannot
            # wait indefinitely for a second cancellation acknowledgement.
            await asyncio.gather(*(asyncio.shield(task) for task in pending),
                                 return_exceptions=True)
            if self.server:
                await self.server.wait_closed()
            if self.http:
                await self.http.aclose()
