"""Apply frozen benchmark thresholds without treating missing data as success."""

from __future__ import annotations

import math
from typing import Any


def metric(summary: dict[str, Any], *keys: str) -> float | bool | None:
    """Read a nested summary value."""
    value: Any = summary
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value if isinstance(value, (int, float, bool)) else None


def numeric_measurement(value: float | bool | None) -> float | None:
    """Return a usable numeric measurement.

    Booleans are not measurements (True must not satisfy a >= gate by
    pretending to be 1), non-finite floats cannot be compared against
    frozen thresholds, and an integer too large for float overflows
    math.isfinite, so all of them are reported as missing instead of
    being used as pass/fail evidence or crashing the evaluation.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    try:
        return value if math.isfinite(value) else None
    except OverflowError:
        return None


def lifecycle_gate(value: float | bool | None) -> bool | None:
    """Return an exact boolean lifecycle value.

    Integer stand-ins such as 1 are not booleans and must be reported as
    missing rather than satisfying the ``== True`` lifecycle gates.
    """
    return value if isinstance(value, bool) else None


def evaluate(
    summary: dict[str, Any], thresholds: dict[str, float], scenario: str = "capacity"
) -> dict[str, Any]:
    """Apply frozen capacity gates, treating missing measurements as inconclusive."""
    input_qps = numeric_measurement(metric(summary, "input_qps"))
    effective_qps = numeric_measurement(metric(summary, "effective_qps"))
    checks = {
        "throughput_ratio": (
            (
                effective_qps / input_qps
                if input_qps is not None
                and input_qps > 0
                and effective_qps is not None
                else None
            ),
            ">=",
            thresholds["min_throughput_ratio"],
        ),
        "http_success_rate": (
            numeric_measurement(metric(summary, "http_success_rate")),
            ">=",
            thresholds["min_http_success_rate"],
        ),
        "trace_completeness": (
            numeric_measurement(metric(summary, "trace_completeness")),
            ">=",
            thresholds["min_trace_completeness"],
        ),
        "token_accuracy": (
            numeric_measurement(metric(summary, "token_accuracy")),
            ">=",
            thresholds["min_token_accuracy"],
        ),
        "latency_p99_ms": (
            numeric_measurement(metric(summary, "latency_ms", "p99")),
            "<=",
            thresholds["max_p99_ms"],
        ),
        "drop_rate": (
            numeric_measurement(metric(summary, "drop_rate")),
            "<=",
            thresholds["max_drop_rate"],
        ),
        "rss_max_mb": (
            numeric_measurement(metric(summary, "resources", "rss_mb", "max")),
            "<=",
            thresholds["max_rss_mb"],
        ),
        "process_survived": (
            lifecycle_gate(metric(summary, "process_survived")),
            "==",
            True,
        ),
        "runtime_clean": (
            lifecycle_gate(metric(summary, "runtime_clean")),
            "==",
            True,
        ),
    }
    if scenario == "soak":
        checks["rss_slope_mb_per_hour"] = (
            numeric_measurement(
                metric(summary, "resources", "rss_mb", "slope_per_hour")
            ),
            "<=",
            thresholds["max_rss_slope_mb_per_hour"],
        )
    if scenario == "soak":
        checks.update(
            {
                "fd_slope_per_hour": (
                    numeric_measurement(
                        metric(
                            summary, "resources", "file_descriptors", "slope_per_hour"
                        )
                    ),
                    "<=",
                    thresholds["max_fd_slope_per_hour"],
                ),
                "thread_slope_per_hour": (
                    numeric_measurement(
                        metric(summary, "resources", "threads", "slope_per_hour")
                    ),
                    "<=",
                    thresholds["max_thread_slope_per_hour"],
                ),
                "socket_slope_per_hour": (
                    numeric_measurement(
                        metric(
                            summary,
                            "resources",
                            "active_connections",
                            "slope_per_hour",
                        )
                    ),
                    "<=",
                    thresholds["max_socket_slope_per_hour"],
                ),
            }
        )
    missing = [name for name, (value, _, _) in checks.items() if value is None]
    failed = []
    for name, (value, operator, expected) in checks.items():
        if value is None:
            continue
        passed = (
            value >= expected
            if operator == ">="
            else value <= expected if operator == "<=" else value == expected
        )
        if not passed:
            failed.append(name)
    verdict = "INCONCLUSIVE" if missing else "FAIL" if failed else "PASS"
    return {"verdict": verdict, "checks": checks, "missing": missing, "failed": failed}
