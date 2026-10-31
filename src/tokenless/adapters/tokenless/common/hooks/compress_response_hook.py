#!/usr/bin/env python3
"""Tokenless response compression hook for Cosh-NG, Claude Code, Qoder, and OpenCode.

Reads a PostToolUse JSON from stdin, forwards the model-visible tool
response to the unified ``tokenless compress`` Protocol v2 PostTool operation
and translates the result into the host's
envelope. JSON detection, tool threshold selection, TOON selection, and
final acceptance all live behind the entry point; this hook only parses the
host object, declares capabilities, and builds envelopes (§4.5).

One Tokenless subprocess per invocation. Environment-error attribution is
owned by the Rust PostTool service.

Hook point: **PostToolUse**

Output contract per agent:
  - claude-code (>= 2.1.121): the compressed payload *replaces* the
    model-visible tool result via ``hookSpecificOutput.updatedToolOutput``.
    ``additionalContext`` is additive in Claude Code (appended alongside
    the original tool result), so it only carries genuinely additive
    diagnostics (environment attribution). Older Claude Code versions fail
    open: compression is disabled instead of injecting a duplicate payload
    (issue #1645).
  - qoder-cli: ``hookSpecificOutput.updatedToolOutput`` preserves an object
    only when rewrapping native Bash ``stdout``. Qoder renders that field
    without exposing the envelope. Other result shapes retain their JSON
    string replacement. Qoder controls the rendering of preserved fields.
  - qwenwork: ``updatedToolOutput`` preserves structured response types.
  - opencode: the adapter translates ``updatedToolOutput`` to OpenCode's
    mutable ``tool.execute.after`` output.
  - cosh-ng: the compressed payload replaces the response via
    ``hookSpecificOutput.updatedToolResponse``.  Extract only ``llmContent``
    from wrapped responses; never include ``returnDisplay``.  Unsupported
    Cosh-NG versions fail open with compression disabled.
  - other agents (additionalContext-only hosts): passthrough. Additive
    injection would append the compressed copy beside the still-visible
    original — a net token increase — so hosts without true output
    replacement remain passthrough (roadmap §7). Environment attribution is
    still injected: it is additive by design.

For claude-code, qoder-cli, qwenwork, opencode, and cmd, ``llmContent`` and
``returnDisplay`` are ordinary business fields: the complete response reaches
Core. Cosh and legacy diagnostic-only hosts retain their wrapper extraction.

The agent ID is resolved from the host runtime, ``--agent-id`` argument, or
TOKENLESS_AGENT_ID environment variable. When running under Cosh-NG, runtime
detection overrides the declared ID for correct stats attribution. Fallback
paths follow the ANOLISA FHS spec: /usr/bin/tokenless.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hook_utils import (
    _TOKENLESS_FALLBACK,
    _TOKENLESS_LOCAL_LIB,
    _TOKENLESS_LOCAL_SHARE,
    SHELL_TOOLS,
    SKIP_TOOLS,
    build_post_tool_request,
    consume_output_optimization,
    detect_cosh_ng_runtime,
    is_skill_file,
    is_tokenless_retrieve_command,
    parse_version,
    prepare_hook_input,
    resolve_agent_id,
    resolve_binary,
    resolve_tool_call_id,
    run_compress,
    secure_write_text,
    skip,
    tokenless_retrieve_command_available,
    try_parse_json,
    warn,
)

# -- constants ---------------------------------------------------------------

# Shell tool envelopes carry the log in one dominant text field. Qoder uses
# Core's 200-character floor: its smaller build logs were otherwise sent as
# log-blind JSON. Other hosts keep the existing 2000-character rewrap gate.
# Bash Git diffs also need the text slot below either gate.
_SHELL_TEXT_FIELDS = ("stdout", "stderr")
_SHELL_UNWRAP_MIN_CHARS = 2_000
_QODER_SHELL_UNWRAP_MIN_CHARS = 200

# Below the qwen/cosh extension manifests' 10 s host wrapper so a
# pathological input is killed here (fail-open skip) before the host kills
# the whole hook.
_COMPRESS_TIMEOUT = 8

# Claude Code added hookSpecificOutput.updatedToolOutput (normal-path tool
# output replacement for all tools) in v2.1.121. Older versions only support
# the additive additionalContext, which would duplicate the payload.
_CLAUDE_AGENT_ID = "claude-code"
_CLAUDE_MIN_REPLACE_VERSION = (2, 1, 121)
_QODER_AGENT_ID = "qoder-cli"
_QWENWORK_AGENT_ID = "qwenwork"
_OPENCODE_AGENT_ID = "opencode"
# Command Code's afterToolCall returns `content`, which replaces the tool
# result the model sees outright — a true PostToolUse replacement, so no
# version gate is needed the way Claude Code's updatedToolOutput needs one.
_CMD_AGENT_ID = "cmd"
_REPLACE_AGENT_IDS = frozenset(
    {
        _CLAUDE_AGENT_ID,
        _QODER_AGENT_ID,
        _QWENWORK_AGENT_ID,
        _OPENCODE_AGENT_ID,
        _CMD_AGENT_ID,
    }
)

# Cache for `claude --version`, keyed on binary path+mtime+size so upgrades
# invalidate it. Hooks run as a fresh process per tool call and spawning the
# node CLI every time would add noticeable latency.
_CLAUDE_VERSION_CACHE = os.path.join(os.path.expanduser("~"), ".tokenless", ".claude-version")


# -- helpers -------------------------------------------------------------------


def _emit(output: dict) -> None:
    print(json.dumps(output, ensure_ascii=False))


def _emit_attribution_or_skip(env_attribution: str) -> None:
    """Pass the original result through, keeping only additive diagnostics.

    Emits an attribution-only additionalContext when present (it is genuinely
    additive and safe on every agent), otherwise a plain skip. Never returns.
    """
    if env_attribution:
        _emit(
            {
                "suppressOutput": True,
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "additionalContext": env_attribution,
                },
            }
        )
        sys.exit(0)
    skip()


def _shell_text_field(tool_name: str, envelope, agent_id: str) -> tuple | None:
    """The dominant text field of a shell tool's envelope, or ``None``.

    Shell envelopes (``{"stdout": …, "stderr": …}``) are JSON to the entry
    point, which would compress them log-blind. Unwrapping the largest text
    field sends the log itself through the text slot; step 13 re-injects the
    compressed text into a same-shaped envelope, so the host's tool protocol
    is untouched (adapters own envelope knowledge, §4.5). Only the single
    largest field is compressed — one Tokenless subprocess per invocation
    (§5.6) — the other field stays byte-identical in the envelope. The host
    renderer determines which fields become model-visible content.
    """
    if tool_name not in SHELL_TOOLS or not isinstance(envelope, dict):
        return None
    min_chars = (
        _QODER_SHELL_UNWRAP_MIN_CHARS if agent_id == _QODER_AGENT_ID else _SHELL_UNWRAP_MIN_CHARS
    )
    best = None
    for name in _SHELL_TEXT_FIELDS:
        value = envelope.get(name)
        if (
            isinstance(value, str)
            and (
                len(value) >= min_chars
                or (tool_name == "Bash" and name == "stdout" and value.startswith("diff --git "))
            )
            and (best is None or len(value) > len(best[1]))
        ):
            best = (name, value)
    return best


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
            secure_write_text(_CLAUDE_VERSION_CACHE, f"{cache_key}\n{proc.stdout.strip()}")
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


# -- main --------------------------------------------------------------------


def main() -> None:
    # 1. Detect runtime (Cosh-NG vs copilot-shell)
    cosh_ng_version = detect_cosh_ng_runtime()
    cosh_ng_detected = cosh_ng_version is not None

    # 2. Resolve agent ID based on runtime
    agent_id = resolve_agent_id()

    # 3. Read stdin JSON and consume any matching PreTool state.
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        warn("failed to read PostToolUse payload. Passing through unchanged.")
        skip()

    if not isinstance(input_data, dict):
        # A payload that is not a JSON object carries no hook fields; take
        # the same pass-through path as undecodable input instead of
        # raising, as the sibling schema hook already does.
        warn(
            "PostToolUse payload is not a JSON object. "
            "Passing through unchanged."
        )
        skip()

    input_data = prepare_hook_input(input_data, agent_id)
    session_id = input_data.get("session_id", "")
    tool_use_id = resolve_tool_call_id(agent_id, input_data)
    try:
        output_optimization = consume_output_optimization(agent_id, session_id, tool_use_id)
    except OSError as error:
        warn(f"failed to consume PreTool optimization state: {error}")
        output_optimization = "none"

    if cosh_ng_detected and cosh_ng_version == (0, 0, 0):
        warn("Unsupported Cosh-NG version. Response compression disabled (fail open).")
        skip()

    # 4. Resolve the single Core entry point after consuming per-call state.
    tokenless_bin = resolve_binary(
        "tokenless", _TOKENLESS_FALLBACK, _TOKENLESS_LOCAL_SHARE, _TOKENLESS_LOCAL_LIB
    )
    if not tokenless_bin:
        warn("tokenless is not installed. Response compression hook disabled.")
        skip()

    tool_name = input_data.get("tool_name", "unknown")
    tool_response_raw = input_data.get("tool_response", "")
    if not tool_response_raw or tool_response_raw == "{}":
        skip()
    if (
        agent_id == _QWENWORK_AGENT_ID
        and tool_name in {"mcp_list", "mcp_get", "mcp_call"}
        and isinstance(tool_response_raw, dict)
        and {"mcp_operation", "mcp_invocation_path"} <= tool_response_raw.keys()
    ):
        # Desktop MCP wrappers expose telemetry here, not their model-visible result.
        skip()

    # 5. These replacement hosts expose the tool result directly, so fields
    # named llmContent/returnDisplay are business data. Keep Cosh and legacy
    # diagnostic-only hosts' existing wrapper handling.
    # Runtime detection currently resolves agent_id to cosh-ng. Keep the
    # explicit runtime guard so wrapper handling survives precedence changes.
    llm_content = None
    if cosh_ng_detected or agent_id not in _REPLACE_AGENT_IDS:
        if isinstance(tool_response_raw, dict):
            llm_content = tool_response_raw.get("llmContent")
            if llm_content is None:
                llm_content = tool_response_raw.get("returnDisplay")
        elif isinstance(tool_response_raw, str):
            parsed_wrapper = try_parse_json(tool_response_raw)
            if isinstance(parsed_wrapper, dict) and "llmContent" in parsed_wrapper:
                llm_content = parsed_wrapper["llmContent"]

    # The model-visible content we will send for compression
    model_visible_before = llm_content if llm_content is not None else tool_response_raw

    # 6. Skip skill files (YAML frontmatter). Spawn avoidance only: they are
    # never JSON, so the entry point would pass them through anyway.
    if isinstance(model_visible_before, str) and is_skill_file(model_visible_before):
        skip()

    # 7. Copy the model-visible value into the request content (§4.5). A
    # shell envelope's dominant text field goes through the text slot
    # instead of log-blind JSON; ensure_ascii=False matches the entry
    # point's normalization, so size gates measure Unicode characters on
    # both sides.
    text_field = _shell_text_field(tool_name, model_visible_before, agent_id)
    tool_input = input_data.get("tool_input", {})
    grep_content = (
        agent_id == _CLAUDE_AGENT_ID
        and tool_name == "Grep"
        and isinstance(model_visible_before, dict)
        and model_visible_before.get("mode") == "content"
        and isinstance(model_visible_before.get("content"), str)
        and isinstance(tool_input, dict)
        and not any(
            tool_input.get(flag)
            for flag in ("-A", "-B", "-C", "context")
        )
    )
    if grep_content:
        # Native Grep has a text slot inside its schema-checked output object.
        # Context queries retain their existing route until that format is supported.
        text_field = ("content", model_visible_before["content"])
    if text_field is not None:
        content = text_field[1]
    elif isinstance(model_visible_before, str):
        content = model_visible_before
    elif isinstance(model_visible_before, (dict, list)):
        content = json.dumps(model_visible_before, separators=(",", ":"), ensure_ascii=False)
    else:
        skip()

    # 8. Capability declaration: what can this host actually do?
    if cosh_ng_detected:
        can_replace = True
        replace_with_text = True  # updatedToolResponse accepts any text
    elif agent_id in _REPLACE_AGENT_IDS and agent_id != _CLAUDE_AGENT_ID:
        can_replace = True
        # An unwrapped shell field is plain text regardless of its envelope.
        replace_with_text = text_field is not None or not isinstance(
            tool_response_raw, (dict, list)
        )
    elif agent_id == _CLAUDE_AGENT_ID:
        can_replace = _claude_supports_replacement()
        replace_with_text = text_field is not None or not isinstance(
            tool_response_raw, (dict, list)
        )
        if not can_replace:
            warn(
                "Claude Code < 2.1.121 (or version unknown): "
                "updatedToolOutput unsupported, response compression disabled."
            )
    else:
        # additionalContext-only hosts have no true replacement: passthrough
        # (additive injection would duplicate the original — see module doc).
        can_replace = False
        replace_with_text = True

    # 9. Map host facts into the required lifecycle fields.
    if grep_content:
        # A filtered match listing is a tool response, not an authoritative file copy.
        content_origin = "api_response"
    elif tool_name in SKIP_TOOLS:
        content_origin = "file_content"
    elif tool_name in SHELL_TOOLS:
        content_origin = "command_output"
    else:
        content_origin = "api_response"
    # Core reports a plain file print (`cat page.html`) as file_read from the
    # command line, so the page stays verbatim while data still compresses.
    command = None
    if content_origin == "command_output" and isinstance(tool_input, dict):
        command = tool_input.get("command")
    raw_status = str(input_data.get("status", "")).lower()
    shell_process_result = model_visible_before if isinstance(model_visible_before, dict) else None
    if (
        shell_process_result is None
        and tool_name in SHELL_TOOLS
        and isinstance(model_visible_before, str)
    ):
        # Some hosts hand shell output over as text instead of a dict:
        # cosh-core's wrap_tool_response always wraps the raw output into a
        # string llmContent, and copilot-shell delivers a plain string
        # envelope. Either way the PostToolUse payload carries no is_error
        # or status marker. A JSON shell envelope inside that text keeps its
        # exit_code / stderr / error fields, so parse it for error
        # detection — v1 classified these hook-side; under Protocol v2
        # the hook must supply the status and Core owns the diagnosis.
        # Deliberately NOT gated on cosh_ng_detected: the classification is
        # host-agnostic by design (restoring it for every host is the point
        # of this change), so do not add a Cosh-NG condition here.
        # TestCopilotShellEnvelopeClassification in
        # tests/test_cosh_ng_compat.py pins the contract for a host running
        # without any Cosh-NG marker.
        parsed_envelope = try_parse_json(model_visible_before)
        if isinstance(parsed_envelope, str):
            parsed_envelope = try_parse_json(parsed_envelope)
        if isinstance(parsed_envelope, dict):
            shell_process_result = parsed_envelope
    shell_process_error = (
        tool_name in SHELL_TOOLS
        and shell_process_result is not None
        and (
            shell_process_result.get("error") is not None
            or (
                shell_process_result.get("exit_code") is not None
                and shell_process_result.get("exit_code") != 0
            )
            or (
                shell_process_result.get("exitCode") is not None
                and shell_process_result.get("exitCode") != 0
            )
        )
    )
    if raw_status in {"interrupted", "denied"}:
        status = raw_status
    elif input_data.get("is_error") is True or (
        isinstance(tool_response_raw, dict) and tool_response_raw.get("isError") is True
    ):
        status = "error"
    elif shell_process_error:
        status = "error"
    else:
        status = "success"

    # Shell envelopes often carry a large stdout alongside the actual failure
    # in a short stderr. Error results are never replaced, so send the error
    # stream to Core for diagnosis while the host keeps the original envelope.
    if status == "error" and tool_name in SHELL_TOOLS and shell_process_result is not None:
        error_parts = []
        for field in ("stderr", "error"):
            value = shell_process_result.get(field)
            if isinstance(value, str) and value.strip():
                error_parts.append(value)
        if error_parts:
            content = "\n".join(error_parts)

    retrieve_result = status == "success" and is_tokenless_retrieve_command(
        tool_name, input_data.get("tool_input")
    )
    retrieval_available = (
        can_replace
        and status == "success"
        and output_optimization == "none"
        and not retrieve_result
        and tokenless_retrieve_command_available()
    )

    # 10. The one Tokenless subprocess: Core owns all PostTool policy.
    request = build_post_tool_request(
        content,
        agent_id,
        tool_name,
        status,
        content_origin,
        output_optimization,
        result_kind="retrieve" if retrieve_result else "tool",
        recovery={"kind": "shell" if retrieval_available else "none"},
        session_id=session_id,
        tool_use_id=tool_use_id,
        replace_output=can_replace,
        replace_with_text=replace_with_text,
        command=command if isinstance(command, str) else None,
    )
    response = run_compress(tokenless_bin, request, _COMPRESS_TIMEOUT, "post_tool")
    env_attribution = response.get("additional_context", "") if response is not None else ""
    if response is None or response.get("disposition") != "applied":
        _emit_attribution_or_skip(env_attribution)

    output_text = response.get("output")
    if not isinstance(output_text, str) or not output_text:
        warn("tokenless compress returned no output. Passing through unchanged.")
        _emit_attribution_or_skip(env_attribution)

    # 11. Envelope construction — dispatch by agent runtime. An unwrapped
    # text field is re-injected into a same-shaped envelope: the compressed
    # text replaces exactly the field that was sent; every other field retains
    # its original value for the host renderer.
    rewrapped = None
    if text_field is not None:
        rewrapped = dict(model_visible_before)
        rewrapped[text_field[0]] = output_text

    if cosh_ng_detected:
        hook_specific = {
            "hookEventName": "PostToolUse",
            "updatedToolResponse": rewrapped if rewrapped is not None else output_text,
        }
        if env_attribution:
            hook_specific["additionalContext"] = env_attribution
        _emit({"suppressOutput": True, "hookSpecificOutput": hook_specific})
        return

    if rewrapped is not None:
        updated_output = rewrapped
    elif replace_with_text:
        updated_output = output_text
    else:
        # Structured slot: the entry point guarantees schema-stable JSON for
        # an applied response. A parse failure means the subprocess boundary
        # was violated — fail open.
        updated_output = try_parse_json(output_text)
        if updated_output is None:
            warn("tokenless compress returned non-JSON for a structured slot.")
            _emit_attribution_or_skip(env_attribution)

    # Qoder's Bash object renderer reads stdout, not arbitrary result fields
    # or a separately compressed stderr. Keep the existing string contract
    # except for the native Bash stdout rewrap verified with Qoder CLI.
    if (
        agent_id == _QODER_AGENT_ID
        and not isinstance(updated_output, str)
        and not (tool_name == "Bash" and text_field is not None and text_field[0] == "stdout")
    ):
        updated_output = (
            json.dumps(rewrapped, separators=(",", ":"), ensure_ascii=False)
            if rewrapped is not None
            else output_text
        )

    hook_output = {
        "hookEventName": "PostToolUse",
        "updatedToolOutput": updated_output,
    }
    if env_attribution:
        hook_output["additionalContext"] = env_attribution
    _emit({"suppressOutput": True, "hookSpecificOutput": hook_output})


if __name__ == "__main__":
    main()
