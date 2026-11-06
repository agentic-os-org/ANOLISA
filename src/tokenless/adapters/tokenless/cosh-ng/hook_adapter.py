"""cosh-ng hook protocol: host inputs, capabilities and output envelopes."""

from __future__ import annotations

import json
import os

from hook_utils import (
    SHELL_TOOLS,
    is_skill_file,
    is_tokenless_retrieve_command,
    parse_version,
    skip,
    tokenless_retrieve_command_available,
    try_parse_json,
    warn,
)
from result_utils import (
    ToolResult,
    content_origin,
    process_result,
    result_status,
    select_text_field,
)


def detect_cosh_ng_runtime() -> tuple | None:
    """Detect if we are running under Cosh-NG and return its version.

    Returns:
        A (major, minor, patch) version tuple if Cosh-NG is detected,
        or (0, 0, 0) if Cosh-NG is detected but version is unknown
        (unsupported), or None if not running under Cosh-NG.

    Detection checks (in order):
      1. ``COSH_NG_VERSION`` environment variable — set by Cosh-NG when
         launching hook processes.
      2. ``COSH_RUNTIME`` environment variable set to ``cosh-ng``.

    The ``(0, 0, 0)`` sentinel means "Cosh-NG detected but version unknown" —
    callers should treat this as "unsupported version" and fail open (disable
    compression) rather than falling back to duplicate injection.
    """
    version_str = os.environ.get("COSH_NG_VERSION", "")
    if version_str:
        ver = parse_version(version_str)
        if ver:
            return ver
        # Detected Cosh-NG but can't parse version — unsupported
        return (0, 0, 0)

    runtime = os.environ.get("COSH_RUNTIME", "")
    if runtime == "cosh-ng":
        # Cosh-NG detected via runtime env var but no version available
        return (0, 0, 0)

    return None


recovery_available = tokenless_retrieve_command_available
is_retrieve_result = is_tokenless_retrieve_command


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
    if detect_cosh_ng_runtime() in (None, (0, 0, 0)):
        warn("Unsupported Cosh-NG version. Response compression disabled (fail open).")
        skip()
    tool = data.get("tool_name", "unknown")
    raw = data.get("tool_response", "")
    value = raw
    if isinstance(raw, dict):
        visible = raw.get("llmContent")
        if visible is None:
            visible = raw.get("returnDisplay")
        if visible is not None:
            value = visible
    elif isinstance(raw, str):
        wrapper = try_parse_json(raw)
        if isinstance(wrapper, dict) and wrapper.get("llmContent") is not None:
            value = wrapper["llmContent"]
    if isinstance(value, str) and is_skill_file(value):
        skip()
    envelope = value
    if tool in SHELL_TOOLS and isinstance(value, str):
        parsed = try_parse_json(value)
        if isinstance(parsed, dict):
            envelope = parsed
    # Keep the legacy structured result route; current llmContent is text.
    minimum = 2000 if isinstance(value, dict) else 200
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
        True,
    )


def response_output(data: dict, result: ToolResult, output: str, message: str) -> dict:
    updated = result.replacement(output)
    # The host only consumes string replacements, including shell JSON.
    if not isinstance(updated, str):
        updated = json.dumps(updated, separators=(",", ":"), ensure_ascii=False)
    specific = {"hookEventName": "PostToolUse", "updatedToolResponse": updated}
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
