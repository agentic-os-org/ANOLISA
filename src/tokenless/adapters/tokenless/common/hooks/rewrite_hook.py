#!/usr/bin/env python3
"""Tokenless command rewriting hook via Protocol v2 PreTool.

Reads a PreToolUse JSON from stdin, forwards shell arguments to
``tokenless compress``, and translates the Core result into the host's
HookOutput envelope.

Hook point: **PreToolUse** — matcher: shell-family tool names
(``Bash``, ``run_shell_command``, ``terminal``, ``Shell``, ``shell``,
``exec``, ``process``).

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
_WORKBUDDY_AGENT_ID = "workbuddy"


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

    # WorkBuddy/CodeBuddy hosts apply ``modifiedInput`` only together with
    # ``permissionDecision: "allow"`` (the official PreToolUse contract and
    # its troubleshooting Q5: with any other decision the tool keeps the
    # original parameters). Protocol v2 routes the rtk run through the
    # Tokenless Core, which deliberately reports Allow and Ask/Default
    # rewrites alike — the hook never learns whether rtk's permission rules
    # attested the command. Emitting "allow" unconditionally would therefore
    # bypass the host permission gate for unattested commands, so WorkBuddy
    # passes the original command through and keeps the host's normal
    # permission flow; users who accept the bypass opt in with
    # TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 (documented in the user guide).
    if (
        _AGENT_ID == _WORKBUDDY_AGENT_ID
        and os.environ.get("TOKENLESS_WORKBUDDY_AUTO_ALLOW") != "1"
    ):
        warn(
            "WorkBuddy rewrite requires permissionDecision allow, which "
            "would bypass the host permission gate for a command Protocol "
            "v2 cannot attest; passing the original command through "
            "(TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 opts into the bypass)."
        )
        skip()

    session_id = input_data.get("session_id", "")
    tool_use_id = resolve_tool_call_id(_AGENT_ID, input_data)
    # The official WorkBuddy/CodeBuddy HookInput (CLI, IDE and Enterprise
    # references alike) carries no tool-call identifier — only session_id,
    # transcript_path, cwd, permission_mode, hook_event_name and the event
    # fields — so the workbuddy rewrite path must not require one. Every
    # other host keeps the ID requirement for its PreTool->PostTool
    # optimization linkage.
    if not tool_use_id and _AGENT_ID != _WORKBUDDY_AGENT_ID:
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

    # The PreTool->PostTool state is keyed by the call ID; PostTool can
    # never consume an ID-less entry (consume returns "none" for an empty
    # ID), so hosts without a call-ID field skip the mark entirely.
    if tool_use_id:
        try:
            mark_rtk_optimized(_AGENT_ID, session_id, tool_use_id)
        except OSError as error:
            warn(f"failed to persist PreTool optimization state: {error}")
            skip()

    # Emit the formats for runtime compatibility:
    # - ``tool_input``: Cosh-NG partial patch (merges with original params)
    # - ``updatedInput``: copilot-shell / Claude Code full replacement
    # - ``modifiedInput``: WorkBuddy/CodeBuddy partial field override
    hook_output = {
        "hookEventName": "PreToolUse",
        "tool_input": {"command": rewritten},
        "updatedInput": updated_input,
    }
    if _AGENT_ID == _WORKBUDDY_AGENT_ID:
        # WorkBuddy/CodeBuddy hosts apply ``modifiedInput`` only together
        # with ``permissionDecision: "allow"`` (official PreToolUse
        # contract), so "allow" is mandatory once a rewrite is emitted.
        # Reaching this point means the user opted into the bypass with
        # TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 (see the gate above); Protocol v2
        # never reports rtk's verdict, so the reason records the opt-in.
        hook_output["modifiedInput"] = {"command": rewritten}
        hook_output["permissionDecisionReason"] = (
            "Tokenless: rtk rewrite auto-allowed via "
            "TOKENLESS_WORKBUDDY_AUTO_ALLOW (host confirmation bypassed)"
        )
        hook_output["permissionDecision"] = "allow"

    print(json.dumps({"hookSpecificOutput": hook_output}))


if __name__ == "__main__":
    main()
