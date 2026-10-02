"""Shared PII scan text helpers for Hermes plugin integrations."""

from __future__ import annotations

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
    try:
        return json_dumps(value)
    except (TypeError, ValueError):
        return str(value)


def text_sha256(text: str) -> str:
    """Return the scan-pii audit hash for *text*."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def extract_user_text(messages: Any, values: dict[str, Any]) -> str:
    """Extract only the current user input from Hermes hook payloads."""
    for key in ("user_message", "user_input", "prompt"):
        value = values.get(key)
        if isinstance(value, str) and value.strip():
            return value

    if not isinstance(messages, list):
        return ""

    for message in reversed(messages):
        role = _message_value(message, "role")
        if role != "user":
            continue
        return _content_to_text(_message_value(message, "content"))
    return ""


def _content_to_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for item in content:
            if isinstance(item, str):
                parts.append(item)
                continue
            text = _message_value(item, "text")
            if isinstance(text, str):
                parts.append(text)
        return "\n".join(parts)
    return ""


def _message_value(message: Any, key: str) -> Any:
    if isinstance(message, dict):
        return message.get(key)
    return getattr(message, key, None)
