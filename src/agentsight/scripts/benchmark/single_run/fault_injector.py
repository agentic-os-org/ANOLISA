#!/usr/bin/env python3
"""Inject malformed HTTPS inputs while checking that the observed process survives."""

from __future__ import annotations

import argparse
import http.client
import json
import math
import socket
import ssl
import time
from pathlib import Path

from collect_metrics import read_status

def tls_socket(host: str, port: int, timeout: float) -> ssl.SSLSocket:
    """Open an unverified TLS socket to the local benchmark server."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection((host, port), timeout=timeout)
    return context.wrap_socket(raw, server_hostname=host)


def exchange(
    host: str,
    port: int,
    payload: bytes,
    timeout: float,
    *,
    read_response: bool = True,
    truncate_sse: bool = False,
) -> str:
    """Send bytes over TLS and classify the peer response without raising."""
    phase = "not_sent"
    try:
        with tls_socket(host, port, timeout) as connection:
            phase = "send_failed"
            connection.sendall(payload)
            phase = "closed"
            if truncate_sse:
                with http.client.HTTPResponse(connection) as stream:
                    stream.begin()
                    if (
                        stream.status != 200
                        or stream.getheader("Content-Type", "").split(";", 1)[0].strip().lower()
                        != "text/event-stream"
                    ):
                        return "not_sse_response"
                    # Bound the prefix: this fault proves response-side data was
                    # received, then closes without draining the stream or DONE.
                    remaining = 65536
                    deadline = time.monotonic() + timeout
                    has_data = False
                    while remaining > 0:
                        budget = deadline - time.monotonic()
                        if budget <= 0:
                            return "sse_prefix_timeout"
                        connection.settimeout(budget)
                        line = stream.readline(remaining + 1)
                        remaining -= len(line)
                        if not line or remaining < 0:
                            break
                        if line.rstrip(b"\r\n") == b"" and has_data:
                            return "truncated_sse_after_data"
                        if line.startswith(b"data:"):
                            data = line[5:].strip()
                            if data == b"[DONE]":
                                return "sse_completed_before_truncation"
                            has_data = has_data or bool(data)
                    return "missing_sse_data_frame"
            if not read_response:
                return "sent_and_closed"
            response = connection.recv(4096)
    except (OSError, ssl.SSLError, http.client.HTTPException) as error:
        return f"{phase}:{type(error).__name__}"
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
            post(valid).replace(f"Content-Length: {len(valid)}".encode(), b"Content-Length: 2"),
            True,
        ),
        "truncated_body": (
            post(valid).replace(f"Content-Length: {len(valid)}".encode(), b"Content-Length: 4096"),
            False,
        ),
        "truncated_sse": (post(valid, b"X-Benchmark-Fault: truncated-sse\r\n"), True),
        "invalid_utf8": (post(b'\xff\xfe{"request_id":"bench-invalid-utf8"}'), True),
        "binary_body": (post(bytes(range(256))), True),
        "oversized_body": (post(b"x" * oversized_bytes), True),
    }


def delivery_failures(outcomes: dict[str, dict[str, int]]) -> list[str]:
    """Find cases with failed connection or incomplete request transmission."""
    return sorted(
        name
        for name, counts in outcomes.items()
        if any(
            count > 0 and outcome.startswith(("not_sent:", "send_failed:", "failed:"))
            for outcome, count in counts.items()
        )
    )


def health_check(host: str, port: int, timeout: float) -> bool:
    """Return whether the mock server still answers after fault injection."""
    request = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    return " 200 " in exchange(host, port, request, timeout)


def process_alive(pid: int | None) -> bool | None:
    """Return process liveness, or None when no process is being watched."""
    return bool(read_status(pid)) if pid else None


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
                host,
                port,
                payload,
                timeout,
                read_response=read_response,
                truncate_sse=name == "truncated_sse",
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
        "sse_truncation_verified": outcomes.get("truncated_sse")
        == {"truncated_sse_after_data": repetitions},
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
    if (
        args.count < 1
        or args.oversized_bytes < 1
        or not math.isfinite(args.timeout)
        or args.timeout <= 0
    ):
        raise SystemExit(
            "count and oversized-bytes must be positive; timeout must be finite and positive"
        )
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
        if report["server_healthy_after"]
        and report["process_alive_after"] is not False
        and report.get("sse_truncation_verified") is True
        and not delivery_failures(report["outcomes"])
        else 1
    )


if __name__ == "__main__":
    raise SystemExit(main())
