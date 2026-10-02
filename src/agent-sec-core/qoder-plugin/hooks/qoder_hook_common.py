#!/usr/bin/env python3
"""Shared helpers for Qoder command hooks."""

import json
import os
import sys
from typing import Any

_HOOK_POLICIES = frozenset({"observe", "warn", "ask", "block"})
_HOOK_POLICY_ALIASES = {"debug": "observe", "deny": "block"}


def env_flag_enabled(name: str, default: bool = True) -> bool:
    """Read a strict true/false environment flag."""
    value = os.environ.get(name)
    if value is None:
        return default
    normalized = value.strip().lower()
    if normalized == "true":
        return True
    if normalized == "false":
        return False
    return default


def normalize_hook_policy(value: object, default: str) -> str:
    """Normalize a hook policy, including supported compatibility aliases."""
    if not isinstance(value, str):
        return default
    normalized = value.strip().lower()
    normalized = _HOOK_POLICY_ALIASES.get(normalized, normalized)
    return normalized if normalized in _HOOK_POLICIES else default


def env_hook_policy(name: str, default: str) -> str:
    """Read and normalize a four-level hook policy environment variable."""
    return normalize_hook_policy(os.environ.get(name), default)


_FIELD_MAP = {
    "trace_id": "trace_id",
    "session_id": "session_id",
    "run_id": "run_id",
    "call_id": "call_id",
    "tool_call_id": "tool_use_id",
}


def load_hook_input() -> dict[str, Any] | None:
    """Read a JSON object from stdin, returning None for invalid hook input."""
    try:
        value = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def dumps_hook_output(value: dict[str, Any]) -> str:
    """Serialize a Qoder HookOutput object."""
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def allow_output() -> str:
    """Return a permissive Qoder HookOutput JSON string."""
    return dumps_hook_output({"decision": "allow"})


def deny_output(reason: str) -> str:
    """Return a blocking Qoder HookOutput JSON string.

    ``reason`` is the internal block justification; ``systemMessage`` is the
    user-visible text that Qoder renders in the terminal when the hook blocks
    a prompt.  Both are set to the same notice so the user can see why their
    input was rejected.
    """
    return dumps_hook_output(
        {"decision": "deny", "reason": reason, "systemMessage": reason}
    )


def pre_tool_decision_output(decision: str, reason: str | None = None) -> str:
    """Return a PreToolUse permission decision HookOutput."""
    payload: dict[str, Any] = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
        }
    }
    if reason:
        payload["hookSpecificOutput"]["permissionDecisionReason"] = reason
    return dumps_hook_output(payload)


def post_tool_output_replacement(
    value: Any, additional_context: str | None = None
) -> str:
    """Return a PostToolUse HookOutput that replaces the tool response."""
    hook_output: dict[str, Any] = {
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": value,
        }
    }
    if additional_context:
        hook_output["hookSpecificOutput"]["additionalContext"] = additional_context
    return dumps_hook_output(hook_output)


def value_to_text(value: Any) -> str:
    """Convert arbitrary hook payload values into stable text for scanning."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    try:
        return json.dumps(value, ensure_ascii=False, sort_keys=True)
    except (TypeError, ValueError):
        return str(value)


def jsonish_value(value: Any) -> Any:
    """Decode JSON strings while leaving ordinary strings and objects intact."""
    if not isinstance(value, str):
        return value
    stripped = value.strip()
    if not stripped:
        return value
    if stripped[0] not in "[{":
        return value
    try:
        return json.loads(stripped)
    except (json.JSONDecodeError, ValueError):
        return value


def trace_context(input_data: dict[str, Any]) -> dict[str, str]:
    """Build canonical trace context from fields directly present on hook input."""
    context: dict[str, str] = {"agent_name": "qoder"}
    for output_key, input_key in _FIELD_MAP.items():
        value = input_data.get(input_key)
        if isinstance(value, str) and value.strip():
            context[output_key] = value.strip()
    return context


def with_trace_context(args: list[str], input_data: dict[str, Any]) -> list[str]:
    """Prepend hidden agent-sec-cli trace-context args to a CLI invocation."""
    return [
        args[0],
        "--trace-context",
        json.dumps(
            trace_context(input_data), ensure_ascii=False, separators=(",", ":")
        ),
        *args[1:],
    ]
