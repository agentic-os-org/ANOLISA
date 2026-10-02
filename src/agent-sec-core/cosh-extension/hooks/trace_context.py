"""Shared trace-context helpers for cosh hook scripts."""

import json
from typing import Any

_FIELD_MAP = {
    "trace_id": "trace_id",
    "session_id": "session_id",
    "run_id": "run_id",
    "call_id": "call_id",
    "tool_call_id": "tool_use_id",
}


def trace_context(input_data: dict[str, Any]) -> dict[str, str]:
    """Build canonical trace context from fields directly present on hook input."""
    context: dict[str, str] = {"agent_name": "cosh"}
    for output_key, input_key in _FIELD_MAP.items():
        value = input_data.get(input_key)
        if isinstance(value, str) and value.strip():
            context[output_key] = value.strip()
    return context


def with_trace_context(args: list[str], input_data: dict[str, Any]) -> list[str]:
    """Prepend hidden agent-sec-cli trace-context args when hook input has tracing."""
    context = trace_context(input_data)
    if context is None:
        return args
    return [
        args[0],
        "--trace-context",
        json.dumps(context, ensure_ascii=False, separators=(",", ":")),
        *args[1:],
    ]
