"""Shared finite-number, percentile, and trend calculations for benchmark reports."""

from __future__ import annotations

import math
from collections import deque
from statistics import fmean
from typing import Any


def numeric(value: Any) -> float | None:
    """Convert a finite number to float, ignoring malformed values."""
    if isinstance(value, bool):
        return None
    try:
        result = float(value)
    except (TypeError, ValueError, OverflowError):
        return None
    return result if math.isfinite(result) else None


def percentile(values: list[float], fraction: float = 0.95) -> float | None:
    """Return the nearest-rank percentile, or None when no samples exist."""
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, math.ceil(len(ordered) * fraction) - 1)
    return ordered[index]


def stats(values: list[float]) -> dict[str, float | None]:
    """Calculate the values most useful when scanning a report."""
    return {
        "first": values[0] if values else None,
        "last": values[-1] if values else None,
        "min": min(values) if values else None,
        "avg": fmean(values) if values else None,
        "p50": percentile(values, 0.50),
        "p95": percentile(values),
        "p99": percentile(values, 0.99),
        "max": max(values) if values else None,
    }


def linear_slope(samples: list[tuple[float, float]]) -> float | None:
    """Return a least-squares slope per hour for timestamped samples."""
    if len(samples) < 2:
        return None
    origin = samples[0][0]
    xs = [timestamp - origin for timestamp, _ in samples]
    ys = [value for _, value in samples]
    mean_x = fmean(xs)
    mean_y = fmean(ys)
    denominator = sum((value - mean_x) ** 2 for value in xs)
    if denominator <= 0:
        return None
    per_second = (
        sum((x_value - mean_x) * (y_value - mean_y) for x_value, y_value in zip(xs, ys))
        / denominator
    )
    return per_second * 3600


def rolling_max_increase(
    samples: list[tuple[float, float]], window_seconds: float = 300
) -> float | None:
    """Return the largest increase between samples at most one window apart."""
    if len(samples) < 2:
        return None
    largest: float | None = None
    minimums: deque[int] = deque()
    for right, (timestamp, value) in enumerate(samples):
        while minimums and value < samples[minimums[-1]][1]:
            minimums.pop()
        minimums.append(right)
        while (
            len(minimums) > 1
            and timestamp - samples[minimums[0]][0] > window_seconds
        ):
            minimums.popleft()
        increase = value - samples[minimums[0]][1]
        largest = increase if largest is None else max(largest, increase)
    return largest
