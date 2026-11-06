"""Run normalized hook operations through the shared Protocol v2 transport."""

import json
import sys
from types import ModuleType

from hook_utils import (
    _TOKENLESS_FALLBACK,
    _TOKENLESS_LOCAL_LIB,
    _TOKENLESS_LOCAL_SHARE,
    build_post_tool_request,
    build_pre_tool_request,
    consume_output_optimization,
    mark_rtk_optimized,
    resolve_binary,
    run_compress,
    skip,
    warn,
)
from result_utils import error_content

_PRE_TOOL_TIMEOUT = 8


def pre_tool(adapter: ModuleType, agent_id: str) -> None:
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        skip()

    if not isinstance(input_data, dict):
        # A payload that is not a JSON object carries no command to
        # rewrite; take the same silent path as undecodable input instead
        # of raising, as the sibling schema hook already does.
        skip()

    input_data = adapter.prepare_input(input_data)
    tokenless_bin = resolve_binary(
        "tokenless",
        _TOKENLESS_FALLBACK,
        _TOKENLESS_LOCAL_SHARE,
        _TOKENLESS_LOCAL_LIB,
    )
    if not tokenless_bin:
        warn("tokenless is not installed. Hook disabled.")
        skip()

    tool_input = input_data.get("tool_input", {})
    if not isinstance(tool_input, dict):
        skip()
    command = tool_input.get("command", "")
    if not isinstance(command, str) or not command:
        skip()

    session_id = input_data.get("session_id", "")
    tool_use_id = adapter.tool_call_id(input_data)
    if not tool_use_id:
        skip()

    tool_name, recovery_input = adapter.pre_tool_input(input_data)
    if recovery_input is not None:
        print(json.dumps(adapter.pre_tool_output(recovery_input)))
        return

    request = build_pre_tool_request(
        tool_input,
        agent_id,
        tool_name,
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
        mark_rtk_optimized(agent_id, session_id, tool_use_id)
    except OSError as error:
        warn(f"failed to persist PreTool optimization state: {error}")
        skip()

    print(json.dumps(adapter.pre_tool_output(updated_input)))


def post_tool(adapter: ModuleType, agent_id: str) -> None:
    try:
        data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        warn("failed to read PostToolUse payload. Passing through unchanged.")
        skip()
    if not isinstance(data, dict):
        warn("PostToolUse payload is not a JSON object. Passing through unchanged.")
        skip()
    data = adapter.prepare_input(data)
    session_id = data.get("session_id", "")
    call_id = adapter.tool_call_id(data)
    try:
        optimization = consume_output_optimization(agent_id, session_id, call_id)
    except OSError as error:
        warn(f"failed to consume PreTool optimization state: {error}")
        optimization = "none"
    binary = resolve_binary(
        "tokenless", _TOKENLESS_FALLBACK, _TOKENLESS_LOCAL_SHARE, _TOKENLESS_LOCAL_LIB
    )
    if not binary:
        warn("tokenless is not installed. Response compression hook disabled.")
        skip()
    raw = data.get("tool_response", "")
    if not raw or raw == "{}":
        skip()
    result = adapter.response(data)
    tool_name = data.get("tool_name", "unknown")
    retrieve = result.status == "success" and adapter.is_retrieve_result(
        tool_name, data.get("tool_input")
    )
    recovery = (
        result.replace_output
        and result.status == "success"
        and optimization == "none"
        and not retrieve
        and adapter.recovery_available()
    )
    request = build_post_tool_request(
        error_content(result),
        agent_id,
        tool_name,
        result.status,
        result.origin,
        optimization,
        result_kind="retrieve" if retrieve else "tool",
        recovery={"kind": "shell" if recovery else "none"},
        session_id=session_id,
        tool_use_id=call_id,
        replace_output=result.replace_output,
        replace_with_text=result.replace_with_text,
        command=result.command if isinstance(result.command, str) else None,
    )
    response = run_compress(binary, request, 8, "post_tool")
    message = response.get("additional_context", "") if response is not None else ""
    output = response.get("output") if response is not None else None
    if response is None or response.get("disposition") != "applied":
        payload = adapter.diagnostic_output(message)
    elif not isinstance(output, str) or not output:
        warn("tokenless compress returned no output. Passing through unchanged.")
        payload = adapter.diagnostic_output(message)
    else:
        payload = adapter.response_output(data, result, output, message)
    print(json.dumps(payload, ensure_ascii=False))


def run_hook(operation: str, adapter: ModuleType, agent_id: str) -> None:
    if operation == "pre_tool":
        pre_tool(adapter, agent_id)
    elif operation == "post_tool":
        post_tool(adapter, agent_id)
    elif operation == "before_model":
        from schema_runtime import before_model

        before_model(adapter, agent_id)
    else:
        raise ValueError(f"Unknown hook operation: {operation}")
