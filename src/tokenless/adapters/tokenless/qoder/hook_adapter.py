"""qoder hook protocol: host inputs, capabilities and output envelopes."""

from __future__ import annotations

import json

from hook_utils import (
    SHELL_TOOLS,
    is_skill_file,
    is_tokenless_retrieve_command,
    skip,
    tokenless_retrieve_command_available,
    warn,
)
from result_utils import (
    ToolResult,
    content_origin,
    process_result,
    result_status,
    select_text_field,
)

recovery_available = tokenless_retrieve_command_available
is_retrieve_result = is_tokenless_retrieve_command
schema_input = None  # This host has no BeforeModel schema replacement contract.


def prepare_input(data: dict) -> dict:
    return data


def tool_call_id(data: dict) -> str:
    return data.get("tool_use_id") or data.get("toolCallId", "")


def pre_tool_input(data: dict) -> tuple[str, dict | None]:
    return data.get("tool_name", ""), None


def pre_tool_output(arguments: dict) -> dict:
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "tool_input": {"command": arguments["command"]},
            "updatedInput": arguments,
        }
    }


def diagnostic_output(message: str) -> dict:
    if not message:
        return {}
    return {
        "suppressOutput": True,
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": message,
        },
    }


def response(data: dict) -> ToolResult:
    tool = data.get("tool_name", "unknown")
    raw = data.get("tool_response", "")
    value = raw
    if isinstance(value, str) and is_skill_file(value):
        skip()
    envelope = value
    minimum = 200
    can_replace = True
    if tool == "Bash" and isinstance(envelope, dict):
        # The native renderer consumes stdout, so no other field is replaceable.
        stdout = envelope.get("stdout")
        field = ("stdout", stdout) if isinstance(stdout, str) else None
        can_replace = field is not None
    else:
        field = (
            select_text_field(envelope, ("stdout", "stderr"), minimum)
            if tool in SHELL_TOOLS
            else None
        )
    arguments = data.get("tool_input", {})
    origin = content_origin(tool)
    command = (
        arguments.get("command")
        if origin == "command_output" and isinstance(arguments, dict)
        else None
    )
    status = result_status(
        data.get("status", ""),
        data.get("is_error") is True
        or (isinstance(raw, dict) and raw.get("isError") is True),
        process_result(value) if tool in SHELL_TOOLS else None,
    )
    return ToolResult(
        value,
        envelope,
        field,
        origin,
        command,
        status,
        can_replace,
        field is not None or not isinstance(raw, (dict, list)),
    )


def response_output(data: dict, result: ToolResult, output: str, message: str) -> dict:
    if not result.replace_output:
        warn("tokenless compress returned an applied result for non-replaceable Qoder output.")
        return diagnostic_output(message)
    updated = result.replacement(output)
    if updated is None:
        warn("tokenless compress returned non-JSON for a structured slot.")
        return diagnostic_output(message)
    # Native Bash renders stdout from an object; other shapes need text.
    if not isinstance(updated, str) and not (
        data.get("tool_name") == "Bash"
        and result.field is not None
        and result.field[0] == "stdout"
    ):
        updated = (
            json.dumps(updated, separators=(",", ":"), ensure_ascii=False)
            if result.field is not None
            else output
        )
    specific = {"hookEventName": "PostToolUse", "updatedToolOutput": updated}
    if message:
        specific["additionalContext"] = message
    return {"suppressOutput": True, "hookSpecificOutput": specific}
