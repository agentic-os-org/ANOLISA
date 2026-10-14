#!/usr/bin/env python3
"""Run independent HTTPS/SSE clients and verify each client's output token rate."""

from __future__ import annotations

import argparse
import gzip
import http.client
import json
import math
import os
import re
import signal
import ssl
import subprocess
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from mock_llm_server import (
    INPUT_TOKENS,
    PREFLIGHT_OUTPUT_TOKENS,
    PREFLIGHT_TOTAL_TOKENS,
    TOKEN_TEXT,
)


def write_json(path: Path, value: dict[str, Any]) -> None:
    """Publish complete barrier and result files for other processes."""
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def read_stream(response: http.client.HTTPResponse, end: float) -> tuple[int, int, dict[str, Any]]:
    """Count verified text tokens, requiring the usage trailer and DONE marker."""
    received = measured = 0
    usage: dict[str, Any] = {}
    while True:
        line = response.readline()
        if not line:
            raise ValueError("SSE ended without [DONE]")
        if not line.startswith(b"data:"):
            continue
        data = line[5:].strip()
        if data == b"[DONE]":
            # Consume the SSE blank line and HTTP terminator before TLS reuse.
            while trailing := response.read(1024):
                if trailing.strip():
                    raise ValueError("unexpected data after [DONE]")
            return received, measured, usage
        event = json.loads(data)
        if "usage" in event:
            usage = event["usage"]
        for choice in event.get("choices", []):
            content = choice.get("delta", {}).get("content", "")
            if content:
                if content != TOKEN_TEXT:
                    raise ValueError("unexpected token fixture; use the token-paced mock server")
                received += 1
                measured += int(time.monotonic() < end)


def wait_for_marker(path: Path, parent_pid: int) -> None:
    """Wait for the coordinator without leaving an orphan at a start barrier."""
    while not path.exists():
        if os.getppid() != parent_pid:
            raise RuntimeError("coordinator exited during capture preparation")
        time.sleep(0.02)


def capture_preflight(connection: http.client.HTTPSConnection, run_id: str, index: int) -> str:
    """Verify a small per-worker TLS call before any measured request is sent."""
    request_id = f"preflight-{run_id}-{index}"
    connection.request(
        "POST",
        "/v1/chat/completions",
        json.dumps(
            {
                "request_id": request_id,
                "model": "benchmark-model",
                "messages": [{"role": "user", "content": "capture readiness"}],
            }
        ),
        headers={
            "Content-Type": "application/json",
            "X-Benchmark-Run-ID": f"preflight-{run_id}",
            "X-Benchmark-Preflight": "1",
        },
    )
    response = connection.getresponse()
    payload = json.loads(response.read())
    if (
        response.status != 200
        or payload.get("id") != request_id
        or payload.get("usage")
        != {
            "prompt_tokens": INPUT_TOKENS,
            "completion_tokens": PREFLIGHT_OUTPUT_TOKENS,
            "total_tokens": PREFLIGHT_TOTAL_TOKENS,
        }
    ):
        raise ValueError("capture preflight response or token usage mismatch")
    return request_id


def worker(args: argparse.Namespace) -> int:
    """Keep one request active per process and write bounded per-request metrics."""
    index = args.worker
    root = args.output_dir
    parent_pid = os.getppid()
    measuring = False
    summary: dict[str, Any] = {
        "agent": index,
        "pid": os.getpid(),
        "output_tokens_in_window": 0,
    }
    url = urlsplit(args.base_url)
    # The mock uses a local, self-signed benchmark certificate.
    context = ssl._create_unverified_context()
    context.set_alpn_protocols(["http/1.1"])
    connection = http.client.HTTPSConnection(
        url.hostname,
        url.port or 443,
        context=context,
        timeout=max(10, args.tokens_per_response / args.tokens_per_second + 10),
    )
    try:
        connection.connect()
        write_json(root / f"ready-{index}.json", {"pid": os.getpid()})
        if args.prepare_capture:
            wait_for_marker(root.parent / "tracer-ready.json", parent_pid)
            request_id = capture_preflight(connection, args.run_id, index)
            write_json(root / f"preflight-{index}.json", {"request_id": request_id})
        wait_for_marker(root / "start.json", parent_pid)
        window = json.loads((root / "start.json").read_text(encoding="utf-8"))
        start, end = window["start"], window["end"]
        time.sleep(max(0, start - time.monotonic()))
        measuring = True
        content = "hello " + "x" * max(0, args.payload_kb * 1024 - 180)
        iteration = 0
        with (root / f"agent-{index}.jsonl").open("w", encoding="utf-8", buffering=1) as log:
            while time.monotonic() < end:
                request_id = f"bench-{args.run_id}-{index}-{iteration}"
                started = time.monotonic()
                connection.request(
                    "POST",
                    "/v1/chat/completions",
                    json.dumps(
                        {
                            "request_id": request_id,
                            "model": "benchmark-model",
                            "stream": True,
                            "messages": [{"role": "user", "content": content}],
                            "metadata": {"agent_id": str(index)},
                        }
                    ),
                    headers={
                        "Content-Type": "application/json",
                        "X-Benchmark-Run-ID": args.run_id,
                    },
                )
                response = connection.getresponse()
                if response.status != 200 or "text/event-stream" not in response.getheader(
                    "Content-Type", ""
                ):
                    raise ValueError(f"expected SSE HTTP 200, received HTTP {response.status}")
                stream_start = time.monotonic()
                received, measured, usage = read_stream(response, end)
                if received != args.tokens_per_response or usage != {
                    "prompt_tokens": INPUT_TOKENS,
                    "completion_tokens": received,
                    "total_tokens": INPUT_TOKENS + received,
                }:
                    raise ValueError(
                        f"token count or usage mismatch: received={received}, usage={usage}"
                    )
                finished = time.monotonic()
                summary["output_tokens_in_window"] += measured
                for metric, value in (
                    ("benchmark_requests", 1),
                    ("benchmark_http_success", 1),
                    ("benchmark_latency", (finished - started) * 1000),
                ):
                    log.write(
                        json.dumps(
                            {
                                "metric": metric,
                                "request_id": request_id,
                                "data": {"value": value, "status": 200},
                                "stream_start": stream_start,
                                "stream_end": finished,
                            }
                        )
                        + "\n"
                    )
                iteration += 1
        summary["completed_requests"] = iteration
    except (OSError, ValueError, RuntimeError, http.client.HTTPException) as error:
        summary["error"] = str(error)
    finally:
        try:
            summary["tokens_per_second"] = summary["output_tokens_in_window"] / args.duration
            write_json(root / f"agent-{index}.json", summary)
            if args.capture_marker is not None and measuring:
                release = args.capture_marker.with_name(args.capture_marker.name + ".release")
                # Exiting before queued TLS events are consumed triggers dead-PID
                # cleanup in AgentSight and can discard the final usage/DONE.
                while not release.exists() and os.getppid() == parent_pid:
                    time.sleep(0.05)
        finally:
            connection.close()
    return int("error" in summary)


def terminate(children: list[subprocess.Popen[bytes]]) -> None:
    """Reap every worker, including clients blocked on a network read."""
    for child in children:
        if child.poll() is None:
            child.terminate()
    deadline = time.monotonic() + 3
    for child in children:
        try:
            child.wait(timeout=max(0.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


def summarize(args: argparse.Namespace, window: dict[str, float]) -> dict[str, Any]:
    """Reconcile distinct PIDs, received output rates and observed stream overlap."""
    agents = []
    transitions: list[tuple[float, int]] = []
    with gzip.open(args.output_dir.parent / "agents.jsonl.gz", "wt", encoding="utf-8") as output:
        for index in range(args.agents):
            agents.append(
                json.loads((args.output_dir / f"agent-{index}.json").read_text(encoding="utf-8"))
            )
            path = args.output_dir / f"agent-{index}.jsonl"
            if not path.exists():
                continue
            with path.open(encoding="utf-8") as log:
                for line in log:
                    output.write(line)
                    item = json.loads(line)
                    if item["metric"] != "benchmark_requests":
                        continue
                    start = max(window["start"], item["stream_start"])
                    end = min(window["end"], item["stream_end"])
                    if end > start:
                        transitions.extend(((start, 1), (end, -1)))
    active = peak = 0
    area = 0.0
    previous = window["start"]
    for timestamp, change in sorted(transitions):
        area += active * (timestamp - previous)
        active += change
        peak = max(peak, active)
        previous = timestamp
    average = area / args.duration
    failed = [
        agent["agent"]
        for agent in agents
        if "error" in agent
        or abs(agent["tokens_per_second"] / args.tokens_per_second - 1) > args.rate_tolerance
    ]
    return {
        "schema_version": 1,
        "requested_agents": args.agents,
        "distinct_agent_pids": len({agent["pid"] for agent in agents}),
        "target_tokens_per_second_per_agent": args.tokens_per_second,
        "tokens_per_response": args.tokens_per_response,
        "token_encoding": "cl100k_base",
        "duration_seconds": args.duration,
        "rate_tolerance": args.rate_tolerance,
        "peak_active_streams": peak,
        "average_active_streams": average,
        "received_output_tokens_per_second": sum(agent["tokens_per_second"] for agent in agents),
        "failed_agents": failed,
        "agents": agents,
        "passed": not failed
        and peak == args.agents
        and average >= args.agents * (1 - args.rate_tolerance),
    }


def run(args: argparse.Namespace) -> int:
    """Synchronize real worker processes before the timed measurement window."""
    import tiktoken

    encoding = tiktoken.get_encoding("cl100k_base")
    if len(encoding.encode(TOKEN_TEXT * args.tokens_per_response)) != args.tokens_per_response:
        raise ValueError("benchmark fixture is not one cl100k_base token per delta")
    if args.check_only:
        return 0
    args.output_dir.mkdir(parents=True, exist_ok=False)
    children: list[subprocess.Popen[bytes]] = []
    try:
        for index in range(args.agents):
            capture_args = (
                ["--capture-marker", str(args.capture_marker)]
                if args.capture_marker is not None
                else []
            )
            if args.prepare_capture:
                capture_args.append("--prepare-capture")
            children.append(
                subprocess.Popen(
                    [
                        sys.executable,
                        str(Path(__file__).resolve()),
                        "--worker",
                        str(index),
                        "--base-url",
                        args.base_url,
                        "--output-dir",
                        str(args.output_dir),
                        "--tokens-per-second",
                        str(args.tokens_per_second),
                        "--tokens-per-response",
                        str(args.tokens_per_response),
                        "--duration",
                        str(args.duration),
                        "--payload-kb",
                        str(args.payload_kb),
                        "--run-id",
                        args.run_id,
                        *capture_args,
                    ]
                )
            )
        deadline = time.monotonic() + 60
        while not all((args.output_dir / f"ready-{i}.json").exists() for i in range(args.agents)):
            if any(child.poll() is not None for child in children) or time.monotonic() > deadline:
                raise RuntimeError(f"clients did not become ready; inspect {args.output_dir}")
            time.sleep(0.05)
        if args.prepare_capture:
            write_json(
                args.output_dir.parent / "clients-ready.json",
                {
                    "pids": [
                        json.loads((args.output_dir / f"ready-{i}.json").read_text())["pid"]
                        for i in range(args.agents)
                    ],
                    "request_ids": [f"preflight-{args.run_id}-{i}" for i in range(args.agents)],
                },
            )
            while not (args.output_dir.parent / "load-release.json").exists():
                if any(child.poll() is not None for child in children):
                    raise RuntimeError("a client exited during capture preparation")
                time.sleep(0.05)
        start = time.monotonic() + args.startup_delay
        window = {"start": start, "end": start + args.duration}
        write_json(args.output_dir / "start.json", window)
        deadline = window["end"] + args.tokens_per_response / args.tokens_per_second + 15
        while any(
            (
                not (args.output_dir / f"agent-{index}.json").exists()
                if args.capture_marker is not None
                else child.poll() is None
            )
            for index, child in enumerate(children)
        ):
            if args.capture_marker is not None and any(
                child.poll() is not None and not (args.output_dir / f"agent-{index}.json").exists()
                for index, child in enumerate(children)
            ):
                raise RuntimeError("a client exited without publishing its load result")
            if time.monotonic() > deadline:
                raise RuntimeError("clients exceeded the stream drain timeout")
            time.sleep(0.05)
        summary = summarize(args, window)
        write_json(args.output_dir.parent / "agent-summary.json", summary)
        print(
            json.dumps({key: value for key, value in summary.items() if key != "agents"}),
            flush=True,
        )
        if args.capture_marker is not None:
            args.capture_marker.touch()
            release = args.capture_marker.with_name(args.capture_marker.name + ".release")
            deadline = time.monotonic() + args.capture_timeout + 30
            while not release.exists():
                if any(child.poll() is not None for child in children):
                    raise RuntimeError("a client exited before capture validation finished")
                if time.monotonic() > deadline:
                    raise RuntimeError("capture validation did not release the clients")
                time.sleep(0.05)
        return int(not summary["passed"])
    finally:
        terminate(children)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agents", type=int, default=32)
    parser.add_argument("--tokens-per-second", type=float, default=100)
    parser.add_argument("--tokens-per-response", type=int, default=1000)
    parser.add_argument("--duration", type=float, default=60)
    parser.add_argument("--payload-kb", type=int, default=4)
    parser.add_argument("--base-url", default="https://127.0.0.1:8443")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--run-id", default="manual")
    parser.add_argument("--startup-delay", type=float, default=5)
    parser.add_argument("--rate-tolerance", type=float, default=0.05)
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="validate arguments and the token fixture",
    )
    parser.add_argument("--worker", type=int, help=argparse.SUPPRESS)
    parser.add_argument("--prepare-capture", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--capture-marker", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--capture-timeout", type=float, default=30, help=argparse.SUPPRESS)
    args = parser.parse_args()
    for name in (
        "agents",
        "tokens_per_second",
        "tokens_per_response",
        "duration",
        "payload_kb",
    ):
        value = getattr(args, name)
        if not math.isfinite(value) or value <= 0:
            parser.error(f"{name.replace('_', '-')} must be finite and positive")
    if (
        not math.isfinite(args.startup_delay)
        or args.startup_delay < 0
        or not 0 < args.rate_tolerance < 1
    ):
        parser.error("startup-delay must be non-negative and rate-tolerance between 0 and 1")
    if not math.isfinite(args.capture_timeout) or args.capture_timeout < 0:
        parser.error("capture-timeout must be finite and non-negative")
    url = urlsplit(args.base_url)
    if (
        url.scheme != "https"
        or not url.hostname
        or url.path not in ("", "/")
        or url.query
        or url.fragment
    ):
        parser.error("base-url must be an HTTPS origin")
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.run_id):
        parser.error("run-id may contain only letters, digits, underscores, and hyphens")
    if args.worker is not None:
        return worker(args)

    def interrupted(_: int, __: object) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    try:
        return run(args)
    except KeyboardInterrupt:
        return 130
    except (ImportError, OSError, ValueError, RuntimeError) as error:
        print(f"agent load failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
