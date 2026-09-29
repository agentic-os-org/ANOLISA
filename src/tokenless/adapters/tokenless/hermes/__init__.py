"""Tokenless lifecycle adapter for Hermes Agent.

Hermes cannot replace tool arguments on older supported releases, so PreTool
blocks a shell call and suggests the Core-rewritten command. PostTool sends the
final model-bound result to Core and applies only the returned disposition.
Schema compression is not available from the Hermes hook surface. Marker-directed
recovery uses Hermes's existing shell tool and the trusted local Tokenless CLI.
Tool Ready remains product-wide hard-disabled.

Activation is controlled by the Hermes plugin system — list ``tokenless`` in
``plugins.enabled`` in ``config.yaml``, or enable via
``hermes plugins enable tokenless``.
"""

from __future__ import annotations

import inspect
import json
import logging
import os
import shlex
import sys
from typing import Any

# Resolve shared hook utilities (common/hooks/) with FHS fallback paths.
# Primary: relative path — realpath needed because install.sh symlinks
# __init__.py into ~/.hermes/plugins/, and plain __file__ points to the
# symlink path; resolving .. from the adapter dir hits common/hooks.
# Fallbacks: system and user FHS paths — needed when the plugin bundle is
# *copied* into ~/.hermes/plugins/tokenless/ (e.g. by the anolisa driver)
# instead of symlinked, so the relative path resolves nowhere.  User-scope
# candidates honor XDG_DATA_HOME (anolisa FsLayout::user prefers it over
# ~/.local/share).
#
# Trust model (aligned with codex/scripts/rewrite-hook, bash
# is_trusted_file, and Rust is_trusted_path): system FHS paths are
# unconditional; elsewhere the hooks directory, its parent, and the
# hook_utils.py file itself must be owned by the current user or root and
# must not be world-writable.  A candidate that exists but is rejected or
# incomplete does not stop the search — later candidates are still tried,
# and every rejection reason is kept for the final diagnostic.
_HERE = os.path.dirname(os.path.realpath(__file__))


def _validate_hooks_dir(path: str) -> str | None:
    """Validate a candidate hooks directory for importing hook_utils.

    Returns None when the directory is trusted and contains an importable
    hook_utils.py, otherwise a human-readable rejection reason.
    """
    if not path or not os.path.isabs(path):
        return "not an absolute path"
    real = os.path.realpath(path)
    if not os.path.isdir(real):
        return "directory does not exist"
    module = os.path.join(real, "hook_utils.py")
    if not os.path.isfile(module):
        return "hook_utils.py missing (incomplete or residual install)"
    # System FHS prefixes are always trusted (checked on the realpath so a
    # symlink pointing outside a system prefix cannot bypass the check).
    for prefix in ("/usr/share/", "/usr/local/share/", "/usr/libexec/", "/usr/lib/anolisa/"):
        if real.startswith(prefix):
            return None
    # Outside system prefixes: the hooks dir, its parent, and the module
    # file must be owned by the current uid or root and not world-writable
    # (mirrors bash is_trusted_file / Rust is_trusted_path).
    uid = os.getuid()
    for p in (real, os.path.dirname(real), module):
        try:
            st = os.stat(p)
        except OSError as exc:
            return f"stat failed for {p}: {exc}"
        if st.st_uid != uid and st.st_uid != 0:
            return f"{p} not owned by current user or root (uid {st.st_uid})"
        if st.st_mode & 0o002:
            return f"{p} is world-writable"
    return None


# Symbols that must exist in the shared hook_utils module, paired with the
# exact call shape this adapter uses at its hook entry points.  When a
# candidate passes the trust check but ships an older hook_utils.py (e.g. a
# stale install from a previous adapter version), the candidate is rejected and
# the search continues to later paths.  The lifecycle migration made the Hermes
# adapter a thin Core client, so the required symbols are the Protocol v2
# request builders, the compress runner, and the Retrieve helpers this module
# from-imports at load time.  The remaining load-time imports (resolve_binary,
# SHELL_TOOLS, SKIP_TOOLS, and the local-path constants) predate Protocol v2,
# so every module that ships the v2 API also ships them.  A module missing any
# listed symbol would pass a narrower check and then raise ImportError at the
# top-level from-import, after the candidate search has already ended — with no
# fallback to later complete candidates.
#
# Symbol names alone are not enough: hook_utils has also changed *call
# signatures* while keeping names stable.  ``build_post_tool_request`` took a
# keyword-only ``retrieval_available`` flag until c2c7e580e replaced it with
# the ``recovery`` mapping, so the module shipped by 9f109d559 exports all five
# required symbols yet rejects the ``recovery=`` this adapter passes — the
# plugin would import cleanly and every PostTool request would then die with
# "unexpected keyword argument 'recovery'", again after the candidate search
# has ended and with no fallback.  Each shape is therefore bound with
# :func:`inspect.signature`; the values below are placeholders mirroring the
# real call sites (``bind`` only checks arity and parameter names, so nothing
# is ever called here).
_HOOK_UTILS_CALL_SHAPES: tuple[tuple[str, tuple[Any, ...], dict[str, Any]], ...] = (
    # on_compress_pre_tool:
    #   build_pre_tool_request(args, AGENT_ID, tool_name, "command",
    #                          session_id, tool_call_id,
    #                          replace_arguments=False, block_and_suggest=True)
    (
        "build_pre_tool_request",
        ({}, "", "", "", "", ""),
        {"replace_arguments": False, "block_and_suggest": True},
    ),
    # on_transform_tool_result:
    #   build_post_tool_request(content, AGENT_ID, tool_name, protocol_status,
    #                           content_origin, output_optimization,
    #                           result_kind=..., recovery=..., session_id=...,
    #                           tool_use_id=..., replace_output=True,
    #                           replace_with_text=True)
    (
        "build_post_tool_request",
        ("", "", "", "", "", ""),
        {
            "result_kind": "tool",
            "recovery": {"kind": "none"},
            "session_id": "",
            "tool_use_id": "",
            "replace_output": True,
            "replace_with_text": True,
        },
    ),
    # Both hooks: run_compress(tokenless_bin, request, timeout, operation)
    ("run_compress", ("", {}, 0, ""), {}),
    # on_transform_tool_result: is_tokenless_retrieve_command(tool_name, args)
    ("is_tokenless_retrieve_command", ("", {}), {}),
    # on_transform_tool_result: is_file_read_command(tool_name, args)
    ("is_file_read_command", ("", {}), {}),
    # on_transform_tool_result: tokenless_retrieve_command_available()
    ("tokenless_retrieve_command_available", (), {}),
)

_HOOK_UTILS_REQUIRED_SYMBOLS = tuple(name for name, _, _ in _HOOK_UTILS_CALL_SHAPES)


def _restore_cached_hook_utils(saved: Any) -> None:
    """Reinstate the module cached before a trial import, or drop the trial."""
    if saved is not None:
        sys.modules["hook_utils"] = saved
    else:
        sys.modules.pop("hook_utils", None)


def _call_shape_rejection(
    symbol: Any, name: str, args: tuple[Any, ...], kwargs: dict[str, Any]
) -> str | None:
    """Return why *symbol* cannot take the adapter call shape, else ``None``."""
    try:
        signature = inspect.signature(symbol)
    except (TypeError, ValueError) as exc:
        return f"{name} is not an introspectable callable ({exc})"
    try:
        signature.bind(*args, **kwargs)
    except TypeError as exc:
        # Name the shape the adapter needs, not only what the module offers:
        # a renamed keyword (retrieval_available -> recovery) reports as a
        # missing argument on some Python versions, which alone would not tell
        # the reader which side of the contract moved.
        return (
            f"{name}{signature} rejects the adapter call shape "
            f"({len(args)} positional, kwargs: {', '.join(sorted(kwargs)) or 'none'}): "
            f"{exc}"
        )
    return None


def _check_api_compat(candidate_dir: str) -> str | None:
    """Trial-import hook_utils from *candidate_dir* and verify its API.

    Returns ``None`` when the module loads, exposes every symbol in
    :data:`_HOOK_UTILS_REQUIRED_SYMBOLS`, and every one of them accepts the
    matching call shape in :data:`_HOOK_UTILS_CALL_SHAPES`; otherwise a
    human-readable rejection reason.  On success the freshly imported module is
    kept in ``sys.modules`` so the subsequent ``from hook_utils import …``
    reuses it rather than a stale cached copy.  On rejection the ``sys.path``
    mutation is cleaned up and the previously cached module (if any) is
    restored so later candidates start from a clean state.
    """
    sys.path.insert(0, candidate_dir)
    saved = sys.modules.pop("hook_utils", None)
    try:
        import hook_utils as _trial  # type: ignore[import-not-found]
        missing = [s for s in _HOOK_UTILS_REQUIRED_SYMBOLS
                   if not hasattr(_trial, s)]
        if missing:
            _restore_cached_hook_utils(saved)
            return f"API mismatch: missing {', '.join(missing)}"
        mismatched = []
        for name, args, kwargs in _HOOK_UTILS_CALL_SHAPES:
            reason = _call_shape_rejection(getattr(_trial, name), name, args, kwargs)
            if reason is not None:
                mismatched.append(reason)
        if mismatched:
            _restore_cached_hook_utils(saved)
            return "API mismatch: " + "; ".join(mismatched)
        # Success — keep the freshly imported module in sys.modules.
        return None
    except Exception as exc:
        _restore_cached_hook_utils(saved)
        return f"import failed: {exc}"
    finally:
        sys.path.pop(0)



def _resolve_hook_utils() -> tuple[str, list[str]]:
    """Locate a trusted shared hooks directory and make it importable.

    Returns ``(resolved_path, candidate_list)``.  The resolved path is
    inserted at the front of ``sys.path`` so the shared ``hook_utils``
    module can be imported.  Raises :exc:`ImportError` when no candidate
    passes both the trust policy and the API compatibility check.
    """
    # Resolve real home from passwd DB for user-install fallback path
    # (NOT $HOME — env-controllable).
    try:
        import pwd as _pwd

        real_home = _pwd.getpwuid(os.getuid()).pw_dir
    except (ImportError, KeyError):
        real_home = ""
    if not os.path.isabs(real_home):
        real_home = ""

    candidates = [
        # Source-tree / symlink install.
        os.path.join(_HERE, "..", "common", "hooks"),
        "/usr/share/anolisa/adapters/tokenless/common/hooks",  # RPM system
        "/usr/local/share/anolisa/adapters/tokenless/common/hooks",  # Manual system
    ]
    # XDG user data dir first (anolisa FsLayout::user precedence), then the
    # passwd-home default. XDG_DATA_HOME is env-controllable, but candidates
    # still pass the full ownership/permission validation above.
    xdg_data = os.environ.get("XDG_DATA_HOME", "")
    if xdg_data and os.path.isabs(xdg_data):
        candidates.append(
            os.path.join(xdg_data, "anolisa", "adapters", "tokenless", "common", "hooks")
        )
    if real_home:
        candidates.append(
            os.path.join(
                real_home, ".local", "share", "anolisa", "adapters", "tokenless", "common", "hooks"
            )
        )

    rejections: list[str] = []
    for candidate in candidates:
        reason = _validate_hooks_dir(candidate)
        if reason is not None:
            rejections.append(f"  - {candidate}: {reason}")
            continue
        # Trust check passed — verify API compat (version mismatch guard).
        resolved = os.path.realpath(candidate)
        api_reason = _check_api_compat(resolved)
        if api_reason is not None:
            rejections.append(f"  - {candidate}: {api_reason}")
            continue
        sys.path.insert(0, resolved)
        return resolved, candidates

    raise ImportError(
        "tokenless: no trusted shared hook_utils module (common/hooks/) found.\n"
        "Candidates checked (in order):\n" + "\n".join(rejections) + "\n"
        "Note: a candidate may be rejected by the trust policy (ownership or "
        "permissions) even though the path exists — see the reason next to "
        "each path. Install the tokenless common hooks (anolisa install "
        "tokenless) or re-run adapters/tokenless/hermes/scripts/install.sh "
        "from a complete adapter tree."
    )


_HOOK_UTILS_RESOLVED, _HOOK_UTILS_CANDIDATES = _resolve_hook_utils()

from hook_utils import (
    _RTK_FALLBACK,
    _RTK_LOCAL_LIB,
    _RTK_LOCAL_SHARE,
    _TOKENLESS_FALLBACK,
    _TOKENLESS_LOCAL_LIB,
    _TOKENLESS_LOCAL_SHARE,
)
from hook_utils import SHELL_TOOLS as _SHELL_TOOLS_SHARED
from hook_utils import SKIP_TOOLS as _SKIP_TOOLS_SHARED
from hook_utils import (
    build_post_tool_request,
    build_pre_tool_request,
    is_file_read_command,
    is_tokenless_retrieve_command,
    resolve_binary,
    run_compress,
    tokenless_retrieve_command_available,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

AGENT_ID = "hermes-agent"
_COMPRESS_TIMEOUT_SECONDS = 8

_SKIP_TOOLS: set[str] = _SKIP_TOOLS_SHARED | {
    "session_search",
    "list_sessions",
}

# Use shared SHELL_TOOLS directly - all tools (including "terminal") are now
# defined in the unified tool_categories.json
_SHELL_TOOLS: set[str] = _SHELL_TOOLS_SHARED

# ---------------------------------------------------------------------------
# Binary resolution (thin wrapper over shared cached resolve_binary)
# ---------------------------------------------------------------------------

# Hermes-specific fallback paths for the RTK binary.
_RTK_LIB_FALLBACK = "/usr/lib/anolisa/tokenless/rtk"


def _resolve_binary(name: str, fallback: str) -> str | None:
    """Resolve binary with hermes-specific fallback paths (cached via shared)."""
    local_bin = os.path.join(os.path.expanduser("~"), ".local", "bin", name)
    if name == "rtk":
        return resolve_binary(
            name,
            fallback,
            _RTK_LIB_FALLBACK,
            local_bin,
            _RTK_LOCAL_LIB,
            _RTK_LOCAL_SHARE,
        )
    return resolve_binary(name, fallback, local_bin, _TOKENLESS_LOCAL_LIB, _TOKENLESS_LOCAL_SHARE)


def _have(name: str, fallback: str) -> bool:
    return _resolve_binary(name, fallback) is not None


def _protocol_status(status: Any, result: str) -> str | None:
    """Map Hermes status, deriving it for hosts that omit the field."""
    if isinstance(status, str) and status:
        return {
            "ok": "success",
            "success": "success",
            "error": "error",
            "blocked": "denied",
            "denied": "denied",
            "interrupted": "interrupted",
        }.get(status.lower())
    if status not in (None, ""):
        return None
    try:
        parsed = json.loads(result)
    except json.JSONDecodeError:
        return "success"
    return "error" if isinstance(parsed, dict) and parsed.get("error") else "success"


def _content_origin(tool_name: str, args: Any) -> str:
    if tool_name in _SKIP_TOOLS:
        return "file_content"
    if is_file_read_command(tool_name, args):
        return "file_read"
    if tool_name in _SHELL_TOOLS:
        return "command_output"
    return "api_response"


def _output_optimization(args: Any) -> str:
    """Recognize the attributed RTK wrapper in the command Hermes executed."""
    if not isinstance(args, dict):
        return "none"
    command = args.get("command")
    if not isinstance(command, str):
        return "none"
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return "none"
    for index, token in enumerate(tokens):
        wrapper = tokens[index : index + 6]
        if len(wrapper) < 6 or token != "env":
            continue
        if (
            wrapper[1] == f"TOKENLESS_AGENT_ID={AGENT_ID}"
            and wrapper[2].startswith("TOKENLESS_SESSION_ID=")
            and wrapper[3].startswith("TOKENLESS_TOOL_USE_ID=")
            and wrapper[4].startswith("TOKENLESS_DATA_DIR=")
            and os.path.basename(wrapper[5]) == "rtk"
        ):
            return "rtk"
    return "none"


# ---------------------------------------------------------------------------
# Hook callbacks
# ---------------------------------------------------------------------------


def on_session_start(**kwargs: Any) -> None:
    """Record session mapping for stats context."""
    session_id = kwargs.get("session_id", "")
    if session_id:
        os.environ["TOKENLESS_SESSION_ID"] = str(session_id)
        logger.debug("tokenless: session_start session_id=%s", session_id)


def on_pre_tool_call(
    tool_name: str = "",
    args: Any = None,
    task_id: str = "",
    session_id: str = "",
    tool_call_id: str = "",
    **kwargs: Any,
) -> dict[str, str] | None:
    """Ask Core for an RTK rewrite and translate it to a Hermes block."""
    if tool_name not in _SHELL_TOOLS or not isinstance(args, dict):
        return None
    tokenless_bin = _resolve_binary("tokenless", _TOKENLESS_FALLBACK)
    if not tokenless_bin:
        return None
    request = build_pre_tool_request(
        args,
        AGENT_ID,
        tool_name,
        "command",
        str(session_id),
        str(tool_call_id),
        replace_arguments=False,
        block_and_suggest=True,
    )
    result = run_compress(tokenless_bin, request, _COMPRESS_TIMEOUT_SECONDS, "pre_tool")
    if not isinstance(result, dict):
        return None
    rewritten_args = result.get("arguments")
    rewritten = rewritten_args.get("command") if isinstance(rewritten_args, dict) else None
    if (
        result.get("action") != "block_and_suggest"
        or result.get("output_optimization") != "rtk"
        or not isinstance(rewritten, str)
        or rewritten == args.get("command")
    ):
        return None
    logger.info("tokenless: Core rewrote %s", tool_name)
    return {
        "action": "block",
        "message": f"[tokenless:rewrite] Re-execute as: {rewritten}",
    }


def on_transform_tool_result(
    tool_name: str = "",
    args: Any = None,
    result: str = "",
    task_id: str = "",
    session_id: str = "",
    tool_call_id: str = "",
    duration_ms: int = 0,
    status: str = "",
    **kwargs: Any,
) -> str | None:
    """Send one final Hermes result to Core and apply its disposition."""
    if not isinstance(result, str):
        return None
    protocol_status = _protocol_status(status, result)
    if protocol_status is None:
        return None
    tokenless_bin = _resolve_binary("tokenless", _TOKENLESS_FALLBACK)
    if not tokenless_bin:
        return None
    output_optimization = _output_optimization(args)
    retrieve_result = protocol_status == "success" and is_tokenless_retrieve_command(
        tool_name, args
    )

    # Hermes's terminal tool returns a JSON envelope whose `output` field is
    # the model-visible command output. Compress that field so structured JSON
    # produced by the command remains visible to JsonCompressor, then restore
    # the host envelope below. Other tools already expose their model-bound
    # result directly and must keep the existing path.
    shell_envelope = None
    content = result
    if tool_name in _SHELL_TOOLS:
        try:
            parsed_result = json.loads(result)
        except json.JSONDecodeError:
            parsed_result = None
        if isinstance(parsed_result, dict) and isinstance(parsed_result.get("output"), str):
            shell_envelope = parsed_result
            content = parsed_result["output"]

    request = build_post_tool_request(
        content,
        AGENT_ID,
        tool_name,
        protocol_status,
        _content_origin(tool_name, args),
        output_optimization,
        result_kind="retrieve" if retrieve_result else "tool",
        recovery={
            "kind": (
                "shell"
                if (
                    protocol_status == "success"
                    and output_optimization == "none"
                    and not retrieve_result
                    and tokenless_retrieve_command_available()
                )
                else "none"
            )
        },
        session_id=str(session_id),
        tool_use_id=str(tool_call_id),
        replace_output=True,
        replace_with_text=True,
    )
    response = run_compress(tokenless_bin, request, _COMPRESS_TIMEOUT_SECONDS, "post_tool")
    if not isinstance(response, dict):
        return None
    if response.get("disposition") == "applied":
        output = response.get("output")
        if isinstance(output, str):
            logger.info("tokenless: Core optimized %s", tool_name)
            if shell_envelope is not None:
                shell_envelope["output"] = output
                return json.dumps(shell_envelope, ensure_ascii=False)
            return output
        return None
    if response.get("disposition") == "tool_error":
        additional_context = response.get("additional_context")
        if isinstance(additional_context, str) and additional_context:
            return f"{result}\n\n{additional_context}"
    return None


# ---------------------------------------------------------------------------
# Plugin entry point
# ---------------------------------------------------------------------------


def register(ctx: Any) -> None:
    """Register all tokenless hooks with the Hermes plugin system."""

    ctx.register_hook("on_session_start", on_session_start)
    ctx.register_hook("pre_tool_call", on_pre_tool_call)
    ctx.register_hook("transform_tool_result", on_transform_tool_result)

    features = ["pre-tool", "post-tool"] if _have("tokenless", _TOKENLESS_FALLBACK) else []
    logger.info(
        "tokenless: Hermes plugin registered — active features: %s",
        ", ".join(features) if features else "none (install tokenless binary)",
    )
