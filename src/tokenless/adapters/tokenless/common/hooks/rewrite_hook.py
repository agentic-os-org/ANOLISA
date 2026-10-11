#!/usr/bin/env python3
"""Tokenless command rewriting hook via Protocol v2 PreTool.

Reads a PreToolUse JSON from stdin, forwards shell arguments to
``tokenless compress``, and translates the Core result into the host's
HookOutput envelope.

Hook point: **PreToolUse** — matcher: shell-family tool names
(``Bash``, ``run_shell_command``, ``terminal``, ``Shell``, ``shell``,
``exec``, ``process``, ``RunCommand``).  ``RunCommand`` is Trae's
normalized terminal tool name.

The agent ID is resolved from the host runtime, ``--agent-id`` argument, or
TOKENLESS_AGENT_ID environment variable. Fallback paths follow the ANOLISA
FHS spec: /usr/bin/tokenless.
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hook_utils import (
    _TOKENLESS_FALLBACK,
    _TOKENLESS_LOCAL_LIB,
    _TOKENLESS_LOCAL_SHARE,
    build_pre_tool_request,
    mark_rtk_optimized,
    resolve_agent_id,
    resolve_binary,
    resolve_tool_call_id,
    run_compress,
    skip,
    warn,
)

_PRE_TOOL_TIMEOUT = 8
_AGENT_ID = resolve_agent_id()


def main() -> None:
    tokenless_bin = resolve_binary(
        "tokenless",
        _TOKENLESS_FALLBACK,
        _TOKENLESS_LOCAL_SHARE,
        _TOKENLESS_LOCAL_LIB,
    )
    if not tokenless_bin:
        warn("tokenless is not installed. Hook disabled.")
        skip()

    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        skip()

    tool_input = input_data.get("tool_input", {})
    if not isinstance(tool_input, dict):
        skip()
    command = tool_input.get("command", "")
    if not isinstance(command, str) or not command:
        skip()

    session_id = input_data.get("session_id", "")
    tool_use_id = resolve_tool_call_id(_AGENT_ID, input_data)
    if not tool_use_id:
        skip()

    request = build_pre_tool_request(
        tool_input,
        _AGENT_ID,
        input_data.get("tool_name", ""),
        "command",
        session_id=session_id,
        tool_use_id=tool_use_id,
    )
    response = run_compress(tokenless_bin, request, _PRE_TOOL_TIMEOUT, "pre_tool")
    if response is None or response.get("action") != "replace_arguments":
        skip()
    if response.get("output_optimization") != "rtk":
        skip()

    updated_input = response.get("arguments")
    if not isinstance(updated_input, dict):
        skip()
    rewritten = updated_input.get("command")
    if not isinstance(rewritten, str) or not rewritten or rewritten == command:
        skip()

    try:
        mark_rtk_optimized(_AGENT_ID, session_id, tool_use_id)
    except OSError as error:
        warn(f"failed to persist PreTool optimization state: {error}")
        skip()

    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "tool_input": {"command": rewritten},
                    "updatedInput": updated_input,
                },
            }
        )
    )


if __name__ == "__main__":
    main()
