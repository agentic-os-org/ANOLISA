"""cmd hook protocol: host inputs, capabilities and output envelopes."""

from __future__ import annotations

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


def prepare_input(data: dict) -> dict:
    # The mod already normalizes Command Code's `{toolCallId, toolName, input,
    # state}` call object into the protocol's snake_case payload, and it sends
    # the model-visible text it extracted from the result blocks rather than the
    # block envelope. Nothing is left to reshape here.
    return data


def tool_call_id(data: dict) -> str:
    return data.get("tool_use_id") or data.get("tool_call_id", "")


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
    field = None
    if tool in SHELL_TOOLS:
        if (
            tool == "Bash"
            and isinstance(envelope, dict)
            and isinstance(envelope.get("stdout"), str)
            and envelope["stdout"].startswith("diff --git ")
        ):
            field = ("stdout", envelope["stdout"])
        selected = select_text_field(envelope, ("stdout", "stderr"), minimum)
        if selected is not None and (field is None or len(selected[1]) > len(field[1])):
            field = selected
    arguments = data.get("tool_input", {})
    origin = content_origin(tool)
    # `afterToolCall` returns `content`, which replaces the tool result the model
    # sees outright. That is a true PostToolUse replacement and needs no version
    # gate, unlike Claude Code's `updatedToolOutput`.
    can_replace = True
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
    updated = result.replacement(output)
    if updated is None:
        warn("tokenless compress returned non-JSON for a structured slot.")
        return diagnostic_output(message)
    specific = {"hookEventName": "PostToolUse", "updatedToolOutput": updated}
    if message:
        specific["additionalContext"] = message
    return {"suppressOutput": True, "hookSpecificOutput": specific}


def schema_input(data: dict) -> tuple[object, bool, dict]:
    request = data.get("llm_request")
    if not isinstance(request, dict):
        return None, False, {}
    config = request.get("config")
    if isinstance(config, dict) and "tools" in config:
        tools, declared = config["tools"], True
    else:
        tools, declared = request.get("tools"), "tools" in request
    visible = dict(request)
    visible.pop("tools", None)
    if isinstance(config, dict):
        visible["config"] = dict(config)
        visible["config"].pop("tools", None)
    return tools, declared, visible


def schema_output(tools: list) -> dict:
    return {
        "hookSpecificOutput": {
            "hookEventName": "BeforeModel",
            "llm_request": {"config": {"tools": tools}},
        }
    }


def schema_missing_message(data: dict) -> str:
    # Command Code's ModApi exposes two mutating tool hooks and no tool-schema
    # seam, so the mod never registers a schema hook and this is only reachable
    # through the standalone compatibility entrypoint.
    if not isinstance(data.get("llm_request"), dict):
        return (
            "BeforeModel payload carries no llm_request object, so schema "
            "compression cannot find tool declarations. Passing through "
            "unchanged (warned once per session)."
        )
    return (
        "BeforeModel event carries no tool declarations at "
        "llm_request.config.tools or llm_request.tools, so schema "
        "compression was skipped. If the host declares tools, its "
        "event format may not match the hook. Passing through "
        "unchanged (warned once per session)."
    )


def notice_output(message: str) -> dict:
    return {"systemMessage": message}
