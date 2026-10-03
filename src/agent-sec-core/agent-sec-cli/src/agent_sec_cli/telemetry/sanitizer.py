"""Strict scalar projectors for privacy-safe telemetry fields."""

import math
import re
from datetime import datetime, timezone
from enum import StrEnum
from typing import Any

_ERROR_TYPE_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_.]{0,127}$")


class AgentName(StrEnum):
    """Approved agent product names emitted in telemetry."""

    CODEX = "codex"
    COSH = "cosh"
    HERMES = "hermes"
    OPENCLAW = "openclaw"
    QODER = "qoder"
    QWENCODE = "qwencode"


def now_iso() -> str:
    """Return the current UTC timestamp in ISO-8601 format."""
    return datetime.now(timezone.utc).isoformat()


def timestamp_value(value: Any) -> str | None:
    """Return a canonical timezone-aware ISO-8601 timestamp."""
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.strip())
    except ValueError:
        return None
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        return None
    return parsed.isoformat()


def details_dict(value: Any) -> dict[str, Any]:
    """Return *value* when it is a dict, otherwise an empty dict."""
    if isinstance(value, dict):
        return value
    return {}


def result_dict(details: dict[str, Any]) -> dict[str, Any]:
    """Return details.result when it is a dict, otherwise an empty dict."""
    result = details.get("result")
    if isinstance(result, dict):
        return result
    return {}


def string_value(
    value: Any,
    *,
    max_length: int,
    allow_empty: bool = False,
) -> str | None:
    """Return a trimmed, bounded string or ``None`` for an invalid value."""
    if not isinstance(value, str):
        return None
    normalized = value.strip()
    if not normalized and not allow_empty:
        return None
    return normalized[:max_length]


def agent_name_value(value: Any) -> str:
    """Return an approved agent product name or an empty string."""
    if not isinstance(value, str):
        return ""
    try:
        return AgentName(value.strip()).value
    except ValueError:
        return ""


def enum_value(value: Any, allowed: frozenset[str]) -> str | None:
    """Return *value* only when it is an explicitly approved string value."""
    if isinstance(value, str) and value in allowed:
        return value
    return None


def error_type_value(value: Any) -> str | None:
    """Return a bounded structured error type, never an error message."""
    if not isinstance(value, str) or not _ERROR_TYPE_RE.fullmatch(value):
        return None
    return value


def integer_value(value: Any) -> int | None:
    """Return an integer while rejecting booleans and other coercible values."""
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value


def nonnegative_integer_value(value: Any) -> int | None:
    """Return a non-negative integer or ``None``."""
    normalized = integer_value(value)
    if normalized is None or normalized < 0:
        return None
    return normalized


def nonnegative_number_value(value: Any) -> int | float | None:
    """Return a finite non-negative number while rejecting booleans."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if value < 0 or not math.isfinite(value):
        return None
    return value
