"""Shared trace-context helpers for all Qwen Code hook scripts."""

import json
import os
from typing import Any

_AGENT_NAME = "qwen-code"
_MAX_CORRELATION_ID_LENGTH = 256
_FIELD_ALIASES = {
    "trace_id": ("trace_id",),
    "session_id": ("session_id",),
    "run_id": ("run_id", "turn_id"),
    "call_id": ("call_id",),
    "tool_call_id": ("tool_call_id", "tool_use_id"),
}


def _correlation_value(value: Any) -> str | None:
    """Return one normalized, bounded correlation identifier."""
    if not isinstance(value, str):
        return None
    normalized = value.strip()
    if not normalized:
        return None
    return normalized[:_MAX_CORRELATION_ID_LENGTH]


def trace_context(input_data: dict[str, Any]) -> dict[str, str]:
    """Build canonical CLI trace context from Qwen Code hook identifiers.

    Always includes ``agent_name`` so agent-sec-cli can attribute the request
    even when no tracing fields are present.
    """
    context: dict[str, str] = {"agent_name": _AGENT_NAME}
    for output_key, input_keys in _FIELD_ALIASES.items():
        for input_key in input_keys:
            normalized = _correlation_value(input_data.get(input_key))
            if normalized is not None:
                context[output_key] = normalized
                break

    if "session_id" not in context:
        session_id = _correlation_value(os.environ.get("QWEN_CODE_SESSION_ID"))
        if session_id is not None:
            context["session_id"] = session_id
    return context


def with_trace_context(args: list[str], input_data: dict[str, Any]) -> list[str]:
    """Prepend hidden agent-sec-cli trace-context args to *args*.

    Inserts ``--trace-context <json>`` immediately after the command name
    (``args[0]``) so the flag is parsed as a top-level option.
    """
    context = trace_context(input_data)
    return [
        args[0],
        "--trace-context",
        json.dumps(context, ensure_ascii=False, separators=(",", ":")),
        *args[1:],
    ]
