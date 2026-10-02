"""Parse the stable summary fields emitted by h2load."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from benchmark_stats import stats


def duration_ms(value: str) -> float | None:
    """Convert an h2load duration token to milliseconds."""
    match = re.fullmatch(r"([0-9]+(?:\.[0-9]+)?)(us|ms|s)", value)
    if not match:
        return None
    number = float(match.group(1))
    return number * {"us": 0.001, "ms": 1.0, "s": 1000.0}[match.group(2)]


def summarize_h2load(path: Path | None) -> dict[str, Any]:
    """Parse h2load's request, throughput, timeout, and latency summary."""
    result: dict[str, Any] = {
        "available": False,
        "requests": 0,
        "http_success": 0,
        "timeouts": 0,
        "throughput": None,
        "latency": stats([]),
        "latency_metric": "h2load_request_time",
        "latency_label": "h2load request time (ms)",
    }
    if path is None or not path.exists():
        return result
    text = path.read_text(encoding="utf-8", errors="replace")
    requests = re.search(
        r"requests:\s+(\d+) total,\s+\d+ started,\s+\d+ done,\s+"
        r"(\d+) succeeded,\s+\d+ failed,\s+\d+ errored,\s+(\d+) timeout",
        text,
    )
    finished = re.search(r"finished in\s+\S+,\s+([0-9.]+) req/s", text)
    timing = re.search(r"time for request:\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)", text)
    if requests:
        result["available"] = True
        result["requests"] = int(requests.group(1))
        result["http_success"] = int(requests.group(2))
        result["timeouts"] = int(requests.group(3))
    if finished:
        result["throughput"] = float(finished.group(1))
    if timing:
        minimum, maximum, average = (
            duration_ms(timing.group(index)) for index in (1, 2, 3)
        )
        result["latency"] = {
            "first": None,
            "last": None,
            "min": minimum,
            "avg": average,
            "p50": None,
            "p95": None,
            "p99": None,
            "max": maximum,
        }
    return result
