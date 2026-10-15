#!/usr/bin/env python3
"""Inject malformed HTTPS inputs while checking that the observed process survives."""

from __future__ import annotations

import argparse
import json
import socket
import ssl
import time
from pathlib import Path


def tls_socket(host: str, port: int, timeout: float) -> ssl.SSLSocket:
    """Open an unverified TLS socket to the local benchmark server."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection((host, port), timeout=timeout)
    return context.wrap_socket(raw, server_hostname=host)


def exchange(
    host: str, port: int, payload: bytes, timeout: float, *, read_response: bool = True
) -> str:
    """Send bytes over TLS and classify the peer response without raising."""
    try:
        with tls_socket(host, port, timeout) as connection:
            connection.sendall(payload)
            if not read_response:
                return "sent_and_closed"
            response = connection.recv(4096)
    except (TimeoutError, ConnectionError, OSError, ssl.SSLError) as error:
        return f"closed:{type(error).__name__}"
    if not response:
        return "closed_without_response"
    first_line = response.split(b"\r\n", 1)[0].decode("ascii", errors="replace")
    return first_line


def post(body: bytes, extra_headers: bytes = b"") -> bytes:
    """Build a raw HTTP/1.1 POST for malformed-body test cases."""
    return (
        b"POST /v1/chat/completions HTTP/1.1\r\n"
        b"Host: localhost\r\n"
        b"Content-Type: application/json\r\n"
        + f"Content-Length: {len(body)}\r\n".encode()
        + extra_headers
        + b"Connection: close\r\n\r\n"
        + body
    )


def cases(oversized_bytes: int) -> dict[str, tuple[bytes, bool]]:
    """Return protocol faults required by the benchmark plan."""
    valid = b'{"request_id":"bench-fault","stream":true}'
    return {
        "invalid_json": (post(b'{"request_id":'), True),
        "invalid_chunk_size": (
            (
                b"POST /v1/chat/completions HTTP/1.1\r\n"
                b"Host: localhost\r\nTransfer-Encoding: chunked\r\n"
                b"Connection: close\r\n\r\nZZ\r\ninvalid\r\n0\r\n\r\n"
            ),
            True,
        ),
        "content_length_mismatch": (
            post(valid).replace(
                f"Content-Length: {len(valid)}".encode(), b"Content-Length: 2"
            ),
            True,
        ),
        "truncated_body": (
            post(valid).replace(
                f"Content-Length: {len(valid)}".encode(), b"Content-Length: 4096"
            ),
            False,
        ),
        "truncated_sse": (post(valid), False),
        "invalid_utf8": (post(b'\xff\xfe{"request_id":"bench-invalid-utf8"}'), True),
        "binary_body": (post(bytes(range(256))), True),
        "oversized_body": (post(b"x" * oversized_bytes), True),
    }


def health_check(host: str, port: int, timeout: float) -> bool:
    """Return whether the mock server still answers after fault injection."""
    request = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    return " 200 " in exchange(host, port, request, timeout)


def process_alive(pid: int | None) -> bool | None:
    """Return process liveness, or None when no process is being watched."""
    return Path(f"/proc/{pid}").exists() if pid else None


def inject(
    host: str,
    port: int,
    repetitions: int,
    oversized_bytes: int,
    timeout: float,
    watch_pid: int | None,
) -> dict[str, object]:
    """Run every fault case and preserve per-case outcomes."""
    started = time.time()
    before = process_alive(watch_pid)
    outcomes: dict[str, dict[str, int]] = {}
    for name, (payload, read_response) in cases(oversized_bytes).items():
        counts: dict[str, int] = {}
        for _ in range(repetitions):
            outcome = exchange(
                host, port, payload, timeout, read_response=read_response
            )
            counts[outcome] = counts.get(outcome, 0) + 1
        outcomes[name] = counts

    for _ in range(repetitions):
        try:
            connection = tls_socket(host, port, timeout)
            connection.close()
            outcome = "tls_connected_and_closed"
        except (OSError, ssl.SSLError) as error:
            outcome = f"failed:{type(error).__name__}"
        bucket = outcomes.setdefault("tls_connect_then_close", {})
        bucket[outcome] = bucket.get(outcome, 0) + 1

    return {
        "schema_version": 1,
        "started_at_unix": started,
        "ended_at_unix": time.time(),
        "repetitions_per_case": repetitions,
        "oversized_bytes": oversized_bytes,
        "outcomes": outcomes,
        "server_healthy_after": health_check(host, port, timeout),
        "watched_pid": watch_pid,
        "process_alive_before": before,
        "process_alive_after": process_alive(watch_pid),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8443)
    parser.add_argument("--count", type=int, default=1)
    parser.add_argument("--oversized-bytes", type=int, default=9 * 1024 * 1024)
    parser.add_argument("--timeout", type=float, default=2)
    parser.add_argument("--watch-pid", type=int)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.count < 1 or args.oversized_bytes < 1 or args.timeout <= 0:
        raise SystemExit("count, oversized-bytes, and timeout must be positive")
    report = inject(
        args.host,
        args.port,
        args.count,
        args.oversized_bytes,
        args.timeout,
        args.watch_pid,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(report, sort_keys=True))
    return (
        0
        if report["server_healthy_after"] and report["process_alive_after"] is not False
        else 1
    )


if __name__ == "__main__":
    raise SystemExit(main())
