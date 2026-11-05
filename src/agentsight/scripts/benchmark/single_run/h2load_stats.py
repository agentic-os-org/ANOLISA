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
    timing = re.search(r"(?m)^[ \t]*(time for request|request)[ \t]*:[ \t]*([^\r\n]*)", text)
    if requests:
        result["available"] = True
        result["requests"] = int(requests.group(1))
        result["http_success"] = int(requests.group(2))
        result["timeouts"] = int(requests.group(3))
    if finished:
        result["throughput"] = float(finished.group(1))
    if timing:
        # Newer h2load puts median/p95/p99 before mean; legacy output has no percentiles.
        fields = (
            ("min", "max", "avg")
            if timing.group(1) == "time for request"
            else ("min", "max", "p50", "p95", "p99", "avg")
        )
        columns = timing.group(2).split()
        # Require the trailing standard deviation too, without crossing into another row.
        if len(columns) >= len(fields) + 1:
            result["latency"].update(
                (field, duration_ms(value)) for field, value in zip(fields, columns)
            )
    return result
