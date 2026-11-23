"""claude-code hook protocol: host inputs, capabilities and output envelopes."""

from __future__ import annotations

import os
import subprocess

from hook_utils import (
    SHELL_TOOLS,
    is_skill_file,
    is_tokenless_retrieve_command,
    parse_version,
    resolve_binary,
    secure_write_text,
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

_CLAUDE_MIN_REPLACE_VERSION = (2, 1, 121)
_CLAUDE_VERSION_CACHE = os.path.join(
    os.path.expanduser("~"), ".tokenless", ".claude-version"
)


def _cached_claude_version(claude_bin: str) -> tuple | None:
    """Return the Claude Code version tuple, caching `claude --version`."""
    try:
        st = os.stat(claude_bin)
        cache_key = f"{claude_bin}:{int(st.st_mtime)}:{st.st_size}"
    except OSError:
        cache_key = claude_bin

    try:
        with open(_CLAUDE_VERSION_CACHE) as f:
            key, _, ver_str = f.read().strip().partition("\n")
        if key == cache_key:
            return parse_version(ver_str)
    except OSError:
        pass

    try:
        proc = subprocess.run(
            [claude_bin, "--version"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except Exception as e:
        warn(f"claude --version failed: {e}")
        return None
    if proc.returncode != 0:
        return None
    ver = parse_version(proc.stdout)
    if ver:
        try:
            # Same hardened write as other ~/.tokenless state files (0o600,
            # symlink-safe) so the cache stays private on shared HOMEs.
            secure_write_text(
                _CLAUDE_VERSION_CACHE, f"{cache_key}\n{proc.stdout.strip()}"
            )
        except OSError:
            pass
    return ver


def _claude_supports_replacement() -> bool:
    """Whether the running Claude Code supports updatedToolOutput (>= 2.1.121).

    Returns False when the version cannot be determined; the hook then
    declares no replacement capability, so unknown versions never receive a
    duplicate compressed payload through additionalContext.
    """
    claude_bin = resolve_binary("claude")
    if not claude_bin:
        return False
    ver = _cached_claude_version(claude_bin)
    return ver is not None and ver >= _CLAUDE_MIN_REPLACE_VERSION


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
    if (
        tool == "Grep"
        and isinstance(value, dict)
        and value.get("mode") == "content"
        and isinstance(value.get("content"), str)
        and isinstance(arguments, dict)
        and not any(arguments.get(flag) for flag in ("-A", "-B", "-C", "context"))
    ):
        field = ("content", value["content"])
        origin = "api_response"
    can_replace = _claude_supports_replacement()
    if not can_replace:
        warn(
            "Claude Code < 2.1.121 (or version unknown): "
            "updatedToolOutput unsupported, response compression disabled."
        )
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
