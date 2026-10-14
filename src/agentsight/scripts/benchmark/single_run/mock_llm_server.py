#!/usr/bin/env python3
"""Deterministic local LLM HTTPS/SSE server for AgentSight benchmarks.

HTTP/2 support requires the optional ``h2`` package from
``scripts/benchmark/requirements.txt``.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import socket
import ssl
import threading
import time
from collections.abc import Iterator
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, TextIO

INPUT_TOKENS = 12
OUTPUT_TOKENS = 8
TOTAL_TOKENS = INPUT_TOKENS + OUTPUT_TOKENS
PREFLIGHT_OUTPUT_TOKENS = 1
PREFLIGHT_TOTAL_TOKENS = INPUT_TOKENS + PREFLIGHT_OUTPUT_TOKENS
TOKEN_TEXT = " x"  # One cl100k_base token, including when repeated.
ENDPOINTS = {"/v1/chat/completions", "/v1/responses", "/v1/messages"}
RUN_ID_PATTERN = re.compile(r"[A-Za-z0-9_-]+")


class RequestRecorder:
    """Append accepted requests to per-run ledgers without retaining their IDs."""

    def __init__(self, directory: Path | None) -> None:
        self.directory = directory
        self.run_id: str | None = None
        self.handle: TextIO | None = None
        self.lock = threading.Lock()
        if directory is not None:
            directory.mkdir(parents=True, exist_ok=True)

    def record(self, run_id: str | None, request_id: str, http_status: int | None) -> None:
        """Record one accepted request without retaining its ID in memory."""
        if self.directory is None or not run_id or not RUN_ID_PATTERN.fullmatch(run_id):
            return
        line = json.dumps(
            {"request_id": request_id, "http_status": http_status},
            separators=(",", ":"),
        )
        with self.lock:
            if self.run_id != run_id:
                if self.handle is not None:
                    self.handle.close()
                self.handle = (self.directory / f"{run_id}.jsonl").open(
                    "a", encoding="utf-8", buffering=1
                )
                self.run_id = run_id
            if self.handle is not None:
                self.handle.write(line + "\n")

    def close(self) -> None:
        """Close the active per-run ledger owned by the server."""
        with self.lock:
            if self.handle is not None:
                self.handle.close()
            self.handle = None
            self.run_id = None


def header_value(headers: dict[str, str], name: str) -> str | None:
    """Return an HTTP header using case-insensitive matching."""
    lowered = name.lower()
    return next((value for key, value in headers.items() if key.lower() == lowered), None)


def request_id_from(body: bytes, headers: dict[str, str]) -> str:
    """Return the caller ID, keeping benchmark correlation deterministic."""
    try:
        value = json.loads(body).get("request_id")
    except (json.JSONDecodeError, UnicodeDecodeError, AttributeError):
        value = None
    return str(value or header_value(headers, "X-Request-ID") or "bench-missing")


def record_request(
    settings: Any, run_id: str | None, request_id: str, http_status: int | None
) -> None:
    """Forward a request result when benchmark ledger recording is enabled."""
    recorder = getattr(settings, "request_recorder", None)
    if isinstance(recorder, RequestRecorder):
        recorder.record(run_id, request_id, http_status)


def response_events(
    path: str, request_id: str, chunks: int, chunk_bytes: int
) -> list[dict[str, Any]]:
    """Build provider-shaped SSE events with stable token usage."""
    text = ("benchmark response " * ((chunks * chunk_bytes // 20) + 2)).strip()
    pieces = [text[i : i + chunk_bytes] for i in range(0, len(text), chunk_bytes)][:chunks]
    if len(pieces) < chunks:
        pieces.extend(["x" * chunk_bytes] * (chunks - len(pieces)))
    events: list[dict[str, Any]] = []
    for piece in pieces:
        if path == "/v1/responses":
            events.append(
                {
                    "type": "response.output_text.delta",
                    "response_id": request_id,
                    "request_id": request_id,
                    "delta": piece,
                }
            )
        elif path == "/v1/messages":
            events.append(
                {
                    "type": "content_block_delta",
                    "request_id": request_id,
                    "delta": {"type": "text_delta", "text": piece},
                }
            )
        else:
            events.append(
                {
                    "id": request_id,
                    "object": "chat.completion.chunk",
                    "request_id": request_id,
                    "choices": [{"index": 0, "delta": {"content": piece}}],
                }
            )
    if path == "/v1/responses":
        events.append(
            {
                "type": "response.completed",
                "response_id": request_id,
                "request_id": request_id,
                "response": {"status": "completed"},
                "usage": {
                    "input_tokens": INPUT_TOKENS,
                    "output_tokens": OUTPUT_TOKENS,
                    "total_tokens": TOTAL_TOKENS,
                },
            }
        )
    elif path == "/v1/messages":
        events.append(
            {
                "type": "message_delta",
                "request_id": request_id,
                "delta": {"stop_reason": "end_turn"},
                "usage": {"input_tokens": INPUT_TOKENS, "output_tokens": OUTPUT_TOKENS},
            }
        )
    else:
        events.append(
            {
                "id": request_id,
                "object": "chat.completion.chunk",
                "request_id": request_id,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": INPUT_TOKENS,
                    "completion_tokens": OUTPUT_TOKENS,
                    "total_tokens": TOTAL_TOKENS,
                },
            }
        )
    return events


def token_response_events(request_id: str, output_tokens: int) -> Iterator[dict[str, Any]]:
    """Emit one verified token per delta without buffering a complete response."""
    base = {
        "id": request_id,
        "request_id": request_id,
        "object": "chat.completion.chunk",
        "model": "benchmark-model",
    }
    for _ in range(output_tokens):
        yield {**base, "choices": [{"index": 0, "delta": {"content": TOKEN_TEXT}}]}
    yield {
        **base,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": INPUT_TOKENS,
            "completion_tokens": output_tokens,
            "total_tokens": INPUT_TOKENS + output_tokens,
        },
    }


class BenchmarkHandler(BaseHTTPRequestHandler):
    """Serve one benchmark request using server settings attached to the HTTP server."""

    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        if self.path == "/healthz":
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"OK")
            return
        self.send_error(HTTPStatus.NOT_FOUND)

    def do_POST(self) -> None:
        if self.path not in ENDPOINTS:
            self.send_error(HTTPStatus.NOT_FOUND)
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        headers = {k: v for k, v in self.headers.items()}
        request_id = request_id_from(body, headers)
        run_id = header_value(headers, "X-Benchmark-Run-ID")
        preflight = header_value(headers, "X-Benchmark-Preflight") == "1"
        settings = self.server.settings  # type: ignore[attr-defined]
        token_rate = getattr(settings, "tokens_per_second", None)
        if token_rate is not None and self.path != "/v1/chat/completions":
            self.send_error(HTTPStatus.BAD_REQUEST, "token pacing requires chat completions")
            return
        # Persist the correlation ID before replying. Once k6 observes the
        # response, the validator is guaranteed to see the corresponding row.
        record_request(settings, run_id, request_id, HTTPStatus.OK)
        events = (
            token_response_events(request_id, settings.output_tokens)
            if token_rate is not None
            else response_events(self.path, request_id, settings.chunks, settings.chunk_bytes)
        )
        if settings.sse and not preflight:
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            if token_rate is not None:
                self.connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            started = time.monotonic()
            for index, event in enumerate(events):
                if token_rate is not None and index < settings.output_tokens:
                    # Absolute deadlines avoid accumulating serialization/write latency.
                    time.sleep(max(0, started + (index + 1) / token_rate - time.monotonic()))
                self.chunked_write(f"data: {json.dumps(event, separators=(',', ':'))}\n\n".encode())
                if token_rate is None and settings.chunk_delay:
                    time.sleep(settings.chunk_delay)
            self.chunked_write(b"data: [DONE]\n\n")
            self.chunked_write(b"")
            return
        payload = json.dumps(
            {
                "id": request_id,
                "request_id": request_id,
                "object": "chat.completion",
                "usage": {
                    "prompt_tokens" if preflight else "input_tokens": INPUT_TOKENS,
                    "completion_tokens" if preflight else "output_tokens": (
                        PREFLIGHT_OUTPUT_TOKENS if preflight else OUTPUT_TOKENS
                    ),
                    "total_tokens": PREFLIGHT_TOTAL_TOKENS if preflight else TOTAL_TOKENS,
                },
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": TOKEN_TEXT if preflight else "benchmark response",
                        },
                        "finish_reason": "stop",
                    }
                ],
            },
            separators=(",", ":"),
        ).encode()
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def chunked_write(self, data: bytes) -> None:
        """Write one HTTP/1.1 chunk; an empty chunk terminates the stream."""
        if data:
            self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
        else:
            self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def log_message(self, fmt: str, *args: Any) -> None:
        if getattr(self.server, "verbose", False):  # type: ignore[attr-defined]
            super().log_message(fmt, *args)


class BenchmarkHTTPServer(ThreadingHTTPServer):
    """HTTP/1.1 server carrying immutable response settings."""

    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 128

    def process_request(self, request: socket.socket, client_address: Any) -> None:
        """Route ALPN-negotiated HTTP/2 connections to the h2 event loop."""
        if request.selected_alpn_protocol() == "h2":
            thread = threading.Thread(
                target=serve_h2,
                args=(request, self.settings, self.verbose),  # type: ignore[attr-defined]
                daemon=True,
            )
            thread.start()
        else:
            super().process_request(request, client_address)


def serve_h2(sock: socket.socket, settings: Any, verbose: bool) -> None:
    """Serve HTTP/2 streams using python-h2, which keeps h2load real end-to-end."""
    from h2.config import H2Configuration
    from h2.connection import H2Connection
    from h2.events import DataReceived, RequestReceived, StreamEnded

    connection = H2Connection(config=H2Configuration(client_side=False, header_encoding="utf-8"))
    connection.initiate_connection()
    sock.sendall(connection.data_to_send())
    bodies: dict[int, bytearray] = {}
    paths: dict[int, str] = {}
    run_ids: dict[int, str | None] = {}

    def send_stream_data(stream_id: int, frame: bytes, *, end_stream: bool = False) -> None:
        """Send one payload honoring h2 flow control and the frame-size limit.

        python-h2 raises FlowControlError when a send exceeds the remaining
        window and never reads the socket itself, so a response larger than
        the peer's initial window (~64 KiB) can only be delivered by slicing
        to the window, and by reading (and applying) the client's pending
        WINDOW_UPDATE frames whenever the window is exhausted.
        """
        view = memoryview(frame)
        while view:
            window = connection.local_flow_control_window(stream_id)
            if window <= 0:
                data = sock.recv(65535)
                if not data:
                    raise ConnectionError("client closed while flow-control blocked")
                handle_events(connection.receive_data(data))
                continue
            take = min(len(view), window, connection.max_outbound_frame_size)
            connection.send_data(
                stream_id,
                bytes(view[:take]),
                end_stream=end_stream and take == len(view),
            )
            sock.sendall(connection.data_to_send())
            view = view[take:]

    def handle_events(events: list[Any]) -> None:
        for event in events:
            if isinstance(event, RequestReceived):
                headers = dict(event.headers)
                paths[event.stream_id] = headers.get(":path", "")
                run_ids[event.stream_id] = header_value(headers, "X-Benchmark-Run-ID")
                bodies[event.stream_id] = bytearray()
            elif isinstance(event, DataReceived):
                bodies.setdefault(event.stream_id, bytearray()).extend(event.data)
                connection.acknowledge_received_data(event.flow_controlled_length, event.stream_id)
            elif isinstance(event, StreamEnded):
                path = paths.pop(event.stream_id, "")
                run_id = run_ids.pop(event.stream_id, None)
                body = bytes(bodies.pop(event.stream_id, b""))
                if path == "/healthz":
                    send_h2_body(connection, event.stream_id, b"OK", "text/plain")
                elif path in ENDPOINTS:
                    rid = request_id_from(body, {})
                    record_request(settings, run_id, rid, HTTPStatus.OK)
                    response = response_events(path, rid, settings.chunks, settings.chunk_bytes)
                    if settings.sse:
                        connection.send_headers(
                            event.stream_id,
                            [
                                (":status", "200"),
                                ("content-type", "text/event-stream"),
                            ],
                        )
                        # python-h2 buffers frames until data_to_send() is
                        # called; flush after headers and after every chunk
                        # so --chunk-delay-ms paces delivery instead of
                        # delaying one final burst (the HTTP/1.1 path
                        # already flushes per chunk via chunked_write).
                        sock.sendall(connection.data_to_send())
                        for item in response:
                            frame = f"data: {json.dumps(item, separators=(',', ':'))}\n\n".encode()
                            send_stream_data(event.stream_id, frame)
                            if settings.chunk_delay:
                                time.sleep(settings.chunk_delay)
                        send_stream_data(event.stream_id, b"data: [DONE]\n\n", end_stream=True)
                    else:
                        payload = json.dumps(
                            {
                                "request_id": rid,
                                "usage": {
                                    "input_tokens": INPUT_TOKENS,
                                    "output_tokens": OUTPUT_TOKENS,
                                    "total_tokens": TOTAL_TOKENS,
                                },
                            },
                            separators=(",", ":"),
                        ).encode()
                        send_h2_body(connection, event.stream_id, payload, "application/json")
                else:
                    connection.send_headers(event.stream_id, [(":status", "404")], end_stream=True)

    try:
        while True:
            data = sock.recv(65535)
            if not data:
                return
            handle_events(connection.receive_data(data))
            pending = connection.data_to_send()
            if pending:
                sock.sendall(pending)
    except (ConnectionError, OSError):
        if verbose:
            print("HTTP/2 client disconnected", flush=True)
    finally:
        sock.close()


def send_h2_body(connection: Any, stream_id: int, body: bytes, content_type: str) -> None:
    """Send a complete HTTP/2 response."""
    connection.send_headers(
        stream_id,
        [
            (":status", "200"),
            ("content-type", content_type),
            ("content-length", str(len(body))),
        ],
        end_stream=False,
    )
    connection.send_data(stream_id, body, end_stream=True)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8443)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--chunks", type=int, default=10)
    parser.add_argument("--chunk-bytes", type=int, default=64)
    parser.add_argument("--chunk-delay-ms", type=float, default=0)
    parser.add_argument("--tokens-per-second", type=float)
    parser.add_argument("--output-tokens", type=int)
    parser.add_argument("--json", action="store_true", help="serve JSON instead of SSE")
    parser.add_argument(
        "--http2", action="store_true", help="advertise HTTP/2 when h2 is installed"
    )
    parser.add_argument(
        "--request-log-dir",
        type=Path,
        help="append one low-cardinality request ledger per benchmark run",
    )
    parser.add_argument("--verbose", action="store_true")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    if args.chunks < 1 or args.chunk_bytes < 1 or args.chunk_delay_ms < 0:
        raise SystemExit(
            "chunks, chunk-bytes, and chunk-delay-ms must be non-negative (chunks/bytes > 0)"
        )
    if args.tokens_per_second is not None or args.output_tokens is not None:
        if (
            args.tokens_per_second is None
            or not math.isfinite(args.tokens_per_second)
            or args.tokens_per_second <= 0
            or args.output_tokens is None
            or args.output_tokens <= 0
            or args.json
            or args.http2
        ):
            parser.error("positive --tokens-per-second and --output-tokens require HTTP/1.1 SSE")
    recorder = RequestRecorder(args.request_log_dir)
    server = BenchmarkHTTPServer((args.host, args.port), BenchmarkHandler)
    server.settings = argparse.Namespace(  # type: ignore[attr-defined]
        chunks=args.chunks,
        chunk_bytes=args.chunk_bytes,
        chunk_delay=args.chunk_delay_ms / 1000,
        sse=not args.json,
        request_recorder=recorder,
        tokens_per_second=args.tokens_per_second,
        output_tokens=args.output_tokens,
    )
    server.verbose = args.verbose  # type: ignore[attr-defined]
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.cert, args.key)
    if args.http2:
        try:
            import h2  # noqa: F401
        except ImportError as exc:
            server.server_close()
            raise SystemExit(
                "--http2 requires the optional h2 package; install requirements.txt"
            ) from exc
        context.set_alpn_protocols(["h2", "http/1.1"])
    else:
        context.set_alpn_protocols(["http/1.1"])
    server.socket = context.wrap_socket(server.socket, server_side=True)
    print(f"mock LLM server listening on https://{args.host}:{args.port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        return 0
    finally:
        server.server_close()
        recorder.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
