"""qwenwork hook protocol: host inputs, capabilities and output envelopes."""

from __future__ import annotations

import json
import os
import re
import shlex
import sys
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "hooks", "shared"))

import hook_utils
from hook_utils import (
    SHELL_TOOLS,
    _bare_retrieve_hash,
    is_skill_file,
    resolve_data_directory,
    skip,
    tokenless_retrieve_command_available,
    warn,
)
from result_utils import (
    ToolResult,
    content_origin,
    process_result,
    result_status,
)

AGENT_ID = "qwenwork"
schema_input = None  # This host has no BeforeModel schema replacement contract.

_QWENWORK_WORKSPACE_SHELLS = frozenset(
    {
        "mcp__workspace__bash",
        "mcp__qw-builtin__mcp_workspace_bash",
        "mcp__qw-builtin__mcp__workspace__bash",
    }
)
_SHELL_ENVELOPE_KEYS = frozenset(
    {
        "kind",
        "stdout",
        "stderr",
        "exitCode",
        "exit_code",
        "interrupted",
        "duration_ms",
        "durationMs",
        "isImage",
        "error",
        "isError",
    }
)
_MULTIMODAL_BLOCK_TYPES = frozenset(
    {"image", "audio", "resource", "resource_link", "embedded_resource"}
)
_QWENWORK_PRIVATE_RUNTIME = False
_QWENWORK_RECOVERY: dict[str, str] | None = None


def _prepare_qwenwork_recovery(
    plugin_root: str, payload: dict
) -> dict[str, str] | None:
    if sys.platform != "darwin" or not os.path.isabs(plugin_root):
        return None
    root = Path(plugin_root)
    binary = root / "bin/tokenless"
    if not binary.is_file() or not os.access(binary, os.X_OK):
        return None
    required = (root / "hooks/run-hook.sh", root / "hooks/shared/rewrite_hook.py")
    if not all(path.is_file() and os.access(path, os.R_OK) for path in required):
        return None
    session_id = payload.get("session_id")
    call_id = tool_call_id(payload)
    if not isinstance(session_id, str) or not session_id or not call_id:
        return None
    try:
        data_dir = resolve_data_directory()
        hooks = json.loads((root / "hooks/hooks.json").read_text())["hooks"][
            "PreToolUse"
        ]
        covered = set()
        for group in hooks:
            if any(
                hook.get("type") == "command"
                and "rewrite_hook.py" in shlex.split(hook.get("command", ""))
                for hook in group.get("hooks", [])
            ):
                covered.update(
                    tool
                    for tool in {"Bash", *_QWENWORK_WORKSPACE_SHELLS}
                    if re.fullmatch(group.get("matcher", ".*"), tool)
                )
        if covered != {"Bash", *_QWENWORK_WORKSPACE_SHELLS}:
            return None
    except (
        OSError,
        ValueError,
        RuntimeError,
        KeyError,
        TypeError,
        AttributeError,
        re.error,
    ):
        return None
    return {
        "binary": str(binary),
        "TOKENLESS_AGENT_ID": "qwenwork",
        "TOKENLESS_SESSION_ID": session_id,
        "TOKENLESS_TOOL_USE_ID": call_id,
        "TOKENLESS_DATA_DIR": data_dir,
        # Empty values also override any different defaults in the model shell.
        "TOKENLESS_STASH_DB": os.environ.get("TOKENLESS_STASH_DB", ""),
        "DSH_TOKENLESS_STASH_DB": os.environ.get("DSH_TOKENLESS_STASH_DB", ""),
    }


def _qwenwork_retrieve_command(key: str) -> str | None:
    if _QWENWORK_RECOVERY is None:
        return None
    runtime = _QWENWORK_RECOVERY
    arguments = ["/usr/bin/env"]
    for name in (
        "TOKENLESS_AGENT_ID",
        "TOKENLESS_SESSION_ID",
        "TOKENLESS_TOOL_USE_ID",
        "TOKENLESS_DATA_DIR",
        "TOKENLESS_STASH_DB",
        "DSH_TOKENLESS_STASH_DB",
    ):
        arguments.append(f"{name}={runtime[name]}")
    arguments.extend((runtime["binary"], "retrieve", key))
    return " ".join(shlex.quote(argument) for argument in arguments)


def rewrite_qwenwork_retrieve(tool_name: str, arguments: dict) -> dict | None:
    """Anchor a standalone recovery command to the same private runtime as its Hook."""
    command = arguments.get("command")
    if tool_name not in SHELL_TOOLS or not isinstance(command, str):
        return None
    key = _bare_retrieve_hash(command)
    rewritten = _qwenwork_retrieve_command(key) if key is not None else None
    return {**arguments, "command": rewritten} if rewritten is not None else None


def is_retrieve_result(tool_name: str, arguments: object) -> bool:
    """Recognize the exact local recovery command emitted by Tokenless markers."""
    if tool_name not in SHELL_TOOLS or not isinstance(arguments, dict):
        return False
    command = arguments.get("command")
    if not isinstance(command, str):
        return False
    if _bare_retrieve_hash(command) is not None:
        return True
    if _QWENWORK_RECOVERY is None:
        return False
    try:
        arguments = shlex.split(command)
    except ValueError:
        return False
    if not arguments or not re.fullmatch(r"[0-9a-f]{24}", arguments[-1]):
        return False
    # Only our exact command is recovery; pipelines and user-supplied wrappers
    # must not inherit the exemption from response compression.
    return command == _qwenwork_retrieve_command(arguments[-1])


def prepare_hook_input(input_data: dict) -> dict:
    """Map QwenWork workspace shell events to the shared Bash contract."""
    global _QWENWORK_PRIVATE_RUNTIME, _QWENWORK_RECOVERY
    _QWENWORK_RECOVERY = None

    plugin_root = os.environ.get("QODER_PLUGIN_ROOT", "")
    private_bin = os.path.join(plugin_root, "bin")
    if os.path.isabs(plugin_root) and os.path.isdir(private_bin):
        os.environ["PATH"] = private_bin + os.pathsep + os.environ.get("PATH", "")
        hook_utils._resolved_cache.clear()
        # The hook's private PATH is not the model's shell PATH.
        _QWENWORK_PRIVATE_RUNTIME = True

    _QWENWORK_RECOVERY = _prepare_qwenwork_recovery(plugin_root, input_data)

    tool_name = input_data.get("tool_name")
    if not isinstance(tool_name, str) or tool_name not in _QWENWORK_WORKSPACE_SHELLS:
        return input_data

    cwd = input_data.get("cwd")
    if not isinstance(cwd, str) or not os.path.isabs(cwd) or not os.path.isdir(cwd):
        warn("workspace shell hook has no valid cwd. Passing through unchanged.")
        skip()
    # Let Tokenless resolve its default database directory and carry it into
    # RTK commands. Hook state must not default to the workspace either.
    try:
        data_dir = resolve_data_directory()
    except (OSError, ValueError, RuntimeError) as error:
        warn(f"{error}. Passing through unchanged.")
        skip()
    hook_utils._OPTIMIZATION_STATE_DIR = os.path.join(data_dir, "hook-state")
    # Bare retrieval commands do not inherit the hook's runtime environment.
    _QWENWORK_PRIVATE_RUNTIME = True

    normalized = {**input_data, "tool_name": "Bash"}
    response = input_data.get("tool_response")
    if isinstance(response, list):
        if not (
            len(response) == 1
            and isinstance(response[0], dict)
            and set(response[0]) == {"type", "text"}
            and response[0]["type"] == "text"
            and isinstance(response[0]["text"], str)
        ):
            # Preserve multimodal blocks and metadata we cannot round-trip.
            skip()
        normalized["tool_response"] = response[0]["text"]
    return normalized


def recovery_available() -> bool:
    return _QWENWORK_RECOVERY is not None or (
        not _QWENWORK_PRIVATE_RUNTIME and tokenless_retrieve_command_available()
    )


def prepare_input(data: dict) -> dict:
    return prepare_hook_input(data)


def tool_call_id(data: dict) -> str:
    return data.get("tool_use_id") or data.get("toolCallId", "")


def pre_tool_input(data: dict) -> tuple[str, dict | None]:
    name = data.get("tool_name", "")
    recovery_input = rewrite_qwenwork_retrieve(name, data.get("tool_input", {}))
    if (
        os.name == "nt"
        and name == "Bash"
        and os.environ.get("QWENWORK_PLUGIN_HOOK_COMPATIBILITY")
        == "managed-tool-hooks-v1"
    ):
        return "PowerShell", recovery_input
    return name, recovery_input


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


def _valid_zero_exit(value: object) -> bool:
    return value is None or (
        isinstance(value, int) and not isinstance(value, bool) and value == 0
    )


def _replaceable_shell_stdout(envelope: dict, status: str) -> str | None:
    """Return the shell stdout text slot when text replacement loses no execution facts."""
    if status != "success" or not envelope.keys() <= _SHELL_ENVELOPE_KEYS:
        return None
    if envelope.get("kind") not in (None, "completed", "shell"):
        return None
    if envelope.get("interrupted", False) is not False:
        return None
    if envelope.get("isImage", False) is not False:
        return None
    if envelope.get("error") is not None or envelope.get("isError", False) is not False:
        return None
    if not _valid_zero_exit(envelope.get("exitCode")) or not _valid_zero_exit(
        envelope.get("exit_code")
    ):
        return None
    if envelope.get("stderr") not in (None, ""):
        return None
    stdout = envelope.get("stdout")
    return stdout if isinstance(stdout, str) else None


def _is_multimodal_block(item: object) -> bool:
    if not isinstance(item, dict):
        return False
    block_type = item.get("type")
    if not isinstance(block_type, str) or block_type not in _MULTIMODAL_BLOCK_TYPES:
        return False
    return any(
        key in item
        for key in ("data", "mimeType", "resource", "uri", "blob", "source", "url")
    )


def _contains_multimodal_blocks(value: object) -> bool:
    if isinstance(value, list):
        return any(_is_multimodal_block(item) for item in value)
    if isinstance(value, dict) and isinstance(value.get("content"), list):
        return any(_is_multimodal_block(item) for item in value["content"])
    return False


def _is_cjk(codepoint: int) -> bool:
    return (
        0x4E00 <= codepoint <= 0x9FFF
        or 0x3400 <= codepoint <= 0x4DBF
        or 0xF900 <= codepoint <= 0xFAFF
        or 0x20000 <= codepoint <= 0x2A6DF
        or 0x2A700 <= codepoint <= 0x2B73F
        or 0x2B740 <= codepoint <= 0x2B81F
        or 0x2B820 <= codepoint <= 0x2CEAF
        or 0x2CEB0 <= codepoint <= 0x2EBEF
        or 0x30000 <= codepoint <= 0x3134F
        or 0x3100 <= codepoint <= 0x312F
        or 0xAC00 <= codepoint <= 0xD7AF
        or 0x3040 <= codepoint <= 0x309F
        or 0x30A0 <= codepoint <= 0x30FF
    )


def _estimate_tokens(text: str) -> int:
    """Mirror tokenless_protocol::estimate_tokens (heuristic-v1)."""
    cjk = 0
    other = 0
    for character in text:
        if _is_cjk(ord(character)):
            cjk += 1
        else:
            other += 1
    return cjk + (other + 3) // 4


def _delivered_text(value: object) -> str:
    """Project updatedToolOutput through QwenWork's replacementParts() contract."""
    if isinstance(value, str):
        return value
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def _has_net_benefit(original: str, delivered: str, message: str) -> bool:
    extra_chars = len(message) if message else 0
    extra_tokens = _estimate_tokens(message) if message else 0
    return (
        len(delivered) + extra_chars < len(original)
        and _estimate_tokens(delivered) + extra_tokens < _estimate_tokens(original)
    )


def response(data: dict) -> ToolResult:
    tool = data.get("tool_name", "unknown")
    if not isinstance(tool, str):
        tool = "unknown"
    raw = data.get("tool_response", "")
    value = raw
    if (
        tool in {"mcp_list", "mcp_get", "mcp_call"}
        and isinstance(raw, dict)
        and {"mcp_operation", "mcp_invocation_path"} <= raw.keys()
    ):
        # These desktop events contain telemetry, not the model's tool result.
        skip()
    if isinstance(value, str) and is_skill_file(value):
        skip()
    envelope = value
    status = result_status(
        data.get("status", ""),
        data.get("is_error") is True
        or (isinstance(raw, dict) and raw.get("isError") is True),
        process_result(value) if tool in SHELL_TOOLS else None,
    )
    field = None
    can_replace = True
    if tool in SHELL_TOOLS:
        if isinstance(envelope, dict):
            # QwenWork's updatedToolOutput replaces the native text slot directly
            # and JSON-stringifies returned objects; only single-stream stdout can
            # be replaced faithfully as text.
            stdout = _replaceable_shell_stdout(envelope, status)
            field = ("stdout", stdout) if stdout is not None else None
            can_replace = field is not None
        elif not isinstance(envelope, str):
            can_replace = False
    elif _contains_multimodal_blocks(value):
        can_replace = False
    arguments = data.get("tool_input", {})
    origin = content_origin(tool)
    command = (
        arguments.get("command")
        if origin == "command_output" and isinstance(arguments, dict)
        else None
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
        warn(
            "tokenless compress returned an applied result for non-replaceable QwenWork output."
        )
        return diagnostic_output(message)
    if result.field is not None:
        updated: object = output
    else:
        updated = result.replacement(output)
        if updated is None:
            warn("tokenless compress returned non-JSON for a structured slot.")
            return diagnostic_output(message)
    if not _has_net_benefit(result.content(), _delivered_text(updated), message):
        return diagnostic_output(message)
    specific = {"hookEventName": "PostToolUse", "updatedToolOutput": updated}
    if message:
        specific["additionalContext"] = message
    return {"suppressOutput": True, "hookSpecificOutput": specific}
