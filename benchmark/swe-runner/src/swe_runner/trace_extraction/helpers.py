# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Small utility functions and the ExtractionError exception."""

import json
import logging
import re
import time
from datetime import UTC, datetime
from math import floor
from typing import Any

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Exception
# ---------------------------------------------------------------------------


class ExtractionError(Exception):
    """Raised when the extraction process encounters a fatal error."""


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


def ns_to_iso(ns: int) -> str:
    """Convert nanosecond timestamp to ISO-8601 UTC string."""
    dt = datetime.fromtimestamp(ns / 1e9, tz=UTC)
    return dt.isoformat()


def parse_time_value(value: str) -> int:
    """Parse a timestamp string into nanoseconds since the Unix epoch."""
    normalized = value.strip()
    if not normalized:
        raise ExtractionError("timestamp value cannot be empty")

    if normalized.lower() == "now":
        return time.time_ns()

    if normalized.isdigit():
        raw = int(normalized)
        digits = len(normalized)
        if digits <= 10:
            return raw * 1_000_000_000
        if digits <= 13:
            return raw * 1_000_000
        if digits <= 16:
            return raw * 1_000
        return raw

    try:
        dt = datetime.fromisoformat(normalized.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ExtractionError(f"Invalid timestamp value: {value}") from exc

    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=UTC)
    return int(dt.timestamp() * 1e9)


def parse_json_column(raw: str | None) -> list[Any]:
    """Safely parse a JSON column; return [] on failure."""
    if not raw:
        return []
    try:
        val = json.loads(raw)
        return val if isinstance(val, list) else []
    except (json.JSONDecodeError, TypeError):
        return []


def extract_user_text(messages: list[dict[str, Any]]) -> str | None:
    """Extract the initial user message text from the first user-role message."""
    for msg in messages:
        if msg.get("role") == "user":
            parts = msg.get("parts", [])
            if not isinstance(parts, list):
                continue
            for part in parts:
                if not isinstance(part, dict):
                    continue
                content = part.get("content")
                if part.get("type") == "text" and isinstance(content, str) and content:
                    return content
    return None


def extract_issue_id(text: str | None) -> str | None:
    """Extract SWE-bench issue id from prompt text if present."""
    if not text:
        return None
    match = re.search(r"Issue ID:\s*([^\n\r]+)", text)
    if match:
        return match.group(1).strip()
    return None


def sanitize_path_component(value: str | None) -> str:
    """Make a string safe for use as a directory name."""
    if not value:
        return "__unknown__"
    return re.sub(r"[^A-Za-z0-9._-]", "_", value)


def _safe_int(value: object) -> int:
    """Convert optional numeric values to int, defaulting to 0."""
    if value is None:
        return 0
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str) and value.strip():
        return int(value)
    return 0


def _mean(values: list[int]) -> float:
    """Return the arithmetic mean for a non-empty list, else 0.0."""
    if not values:
        return 0.0
    return sum(values) / len(values)


def _trimmed_mean(values: list[int], trim_ratio: float) -> float:
    """Return the trimmed mean for a list, falling back to the mean when needed."""
    if not values:
        return 0.0

    ordered = sorted(values)
    trim_count = floor(len(ordered) * trim_ratio)
    if trim_count == 0 or (trim_count * 2) >= len(ordered):
        return _mean(ordered)
    return _mean(ordered[trim_count:-trim_count])


def _format_metric(value: float) -> str:
    """Format a metric value for CSV output."""
    return f"{value:.2f}"
