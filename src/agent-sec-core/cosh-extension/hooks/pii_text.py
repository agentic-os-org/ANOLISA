"""Shared PII scan text helpers for Cosh hooks."""

import hashlib
import json
from typing import Any


def json_dumps(value: Any) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
        default=str,
    )


def value_to_text(value: Any) -> str:
    """Convert a hook value to the exact text sent to scan-pii."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return json_dumps(value)


def text_sha256(text: str) -> str:
    """Return the scan-pii audit hash for *text*."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()
