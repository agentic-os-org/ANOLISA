#!/usr/bin/env python3
"""Tokenless schema compression hook.

Reads a BeforeModel JSON from stdin, extracts the tools array, forwards it
to the unified ``tokenless compress`` Protocol v2 BeforeModel operation and
writes a HookOutput JSON to stdout.
The entry point returns the original array on no-savings, which this hook
wraps exactly like a compressed one — the historical behavior of the
``compress-schema`` flow it replaces.

Hook point: **BeforeModel**

The agent ID is read from the TOKENLESS_AGENT_ID environment variable
(set by the install action script).
"""

from __future__ import annotations

import contextlib
import json
import os
import sys

try:  # POSIX hosts (cosh / Cosh-NG) — the platforms these hooks target.
    import fcntl
except ImportError:  # pragma: no cover - non-POSIX fallback keeps best-effort
    fcntl = None

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hook_utils import (
    _TOKENLESS_FALLBACK,
    _TOKENLESS_LOCAL_LIB,
    _TOKENLESS_LOCAL_SHARE,
    build_before_model_request,
    resolve_agent_id,
    resolve_binary,
    resolve_tool_call_id,
    run_compress,
    secure_write_text,
    skip,
    warn,
)

# -- constants ---------------------------------------------------------------

_AGENT_ID = resolve_agent_id()

# Below the extension manifests' 10 s host wrapper, so a pathological batch
# is killed here (fail-open skip) instead of racing the host's kill of the
# whole hook (the old subprocess timeout was 10 s against a 10 s wrapper).
_COMPRESS_TIMEOUT = 8

# One marker file holds the session keys that already emitted the "no tool
# declarations" warning — one key per line, most recent last — so the warning
# repeats at most once per session even though BeforeModel fires on every
# model turn. A single-value marker would not survive concurrent sessions
# under one HOME: session B's first warning would overwrite session A's key
# and make A warn again on its next turn. The list is bounded (oldest entries
# trimmed) so it cannot grow without limit; losing an evicted key only
# re-arms the warning for a long-idle session. All reads and writes of the
# marker run under the exclusive sidecar lock below: an unlocked
# read-modify-write lets two hook processes read the same old key set and
# write back over each other, silently losing one session's key.
_NO_TOOLS_WARN_MARKER = os.path.join(
    os.path.expanduser("~"), ".tokenless", ".schema-hook-nowarn-session"
)

# Best-effort dedup bound: far above any plausible number of concurrently
# active sessions sharing one HOME.
_MAX_WARNED_SESSIONS = 64


@contextlib.contextmanager
def _marker_lock():
    """Exclusive advisory lock guarding the marker read-modify-write.

    Two hook processes warning for different sessions at the same time must
    not read the same old key set and write back over each other. The lock
    lives on a dedicated sidecar file that the marker write never replaces,
    so the locked inode cannot be swapped out mid-section, and the kernel
    drops an ``flock`` when its holder exits or crashes, so a dead hook
    process cannot wedge later sessions. On platforms without ``fcntl``, or
    when the lock file cannot be created, this degrades to the historical
    unlocked best-effort behaviour instead of failing the warning.
    """
    if fcntl is None:
        yield
        return
    lock_path = _NO_TOOLS_WARN_MARKER + ".lock"
    fd = -1
    try:
        os.makedirs(os.path.dirname(lock_path), mode=0o700, exist_ok=True)
        if os.path.islink(lock_path):
            os.unlink(lock_path)
        flags = os.O_RDWR | os.O_CREAT
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = os.open(lock_path, flags, 0o600)
        fcntl.flock(fd, fcntl.LOCK_EX)
    except OSError:
        if fd >= 0:
            os.close(fd)
        yield  # lock unavailable — keep the unlocked best-effort behaviour
        return
    try:
        yield
    finally:
        os.close(fd)


# -- helpers -----------------------------------------------------------------


def _session_warn_key(session_id: str) -> str:
    """Normalize the session ID into the dedup key for the no-tools warning.

    Hosts that omit a session ID share one key, so the warning still appears
    at most once for them instead of on every turn.
    """
    key = (session_id or "").strip()
    return key or "<no-session>"


def _read_warned_sessions() -> list:
    """Read the warned session keys (one per line) from the marker file.

    Tolerates a missing or corrupt marker — dedup is best-effort and a miss
    simply means the session warns once (again).
    """
    try:
        with open(_NO_TOOLS_WARN_MARKER, "r", encoding="utf-8") as handle:
            return [line.strip() for line in handle if line.strip()]
    except OSError:
        return []  # first invocation, or unreadable marker
    except UnicodeDecodeError:
        return []  # corrupt marker — rebuild it on the next warning


def _should_warn_no_tools(session_id: str) -> bool:
    """Return True if the no-tools warning has not been emitted for this
    session yet, and record that it is being emitted now.

    Every warned session key stays in the marker (bounded to the newest
    ``_MAX_WARNED_SESSIONS`` entries), so concurrent sessions under one HOME
    do not invalidate each other's dedup state. The check-and-record runs
    inside ``_marker_lock`` so two first warnings racing for different
    sessions serialize instead of overwriting each other's keys. Best-effort
    by design: the marker lives under ``~/.tokenless`` and is read/written
    with the same hardened helpers as other hook state, but any failure
    falls back to warning — observability beats silence, and the warning
    itself never affects the pass-through behaviour.
    """
    key = _session_warn_key(session_id)
    with _marker_lock():
        warned = _read_warned_sessions()
        if key in warned:
            return False
        warned.append(key)
        warned = warned[-_MAX_WARNED_SESSIONS:]
        try:
            secure_write_text(
                _NO_TOOLS_WARN_MARKER, "".join(entry + "\n" for entry in warned)
            )
        except OSError:
            pass  # state dir unwritable — still warn this once
        return True


def _skip_with_message(msg: str) -> None:
    """Pass through unchanged while surfacing ``msg`` to the user.

    ``systemMessage`` is the protocol field both hosts surface for humans:
    Cosh-NG renders it as a hook notification and copilot-shell records it in
    its hook log, while neither injects it into the model context. The
    warning is also printed to stderr so hosts that forward hook stderr get
    the same text.
    """
    warn(msg)
    print(json.dumps({"systemMessage": msg}))
    sys.exit(0)


def _extract_tools(input_data: dict) -> tuple[object, bool]:
    """Extract the tool declarations from a BeforeModel payload.

    Returns ``(tools, declared)`` where ``declared`` is True when the host
    carried a tools field at any known position — even an empty one.

    ``config.tools`` is the canonical position (both copilot-shell's Hook
    Translator and Cosh-NG put it there); the top-level ``tools`` is the
    older position, kept for hosts that still emit it. Presence of the
    canonical key decides, not its truthiness: a host that declares no tools
    sends an empty canonical array, and falling through to a stale legacy
    field there would compress declarations this request never carried. This
    mirrors the host-side precedence.
    """
    llm_request = input_data.get("llm_request")
    if not isinstance(llm_request, dict):
        return None, False
    config = llm_request.get("config")
    if isinstance(config, dict) and "tools" in config:
        return config["tools"], True
    if "tools" in llm_request:
        return llm_request.get("tools"), True
    return None, False


# -- main --------------------------------------------------------------------


def main() -> None:
    # 1. Check tokenless binary
    tokenless_bin = resolve_binary(
        "tokenless",
        _TOKENLESS_FALLBACK,
        _TOKENLESS_LOCAL_SHARE,
        _TOKENLESS_LOCAL_LIB,
    )
    if not tokenless_bin:
        warn(
            "tokenless is not installed or not in PATH. Schema compression hook disabled."
        )
        skip()

    # 2. Read stdin JSON
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        warn("failed to read BeforeModel payload. Passing through unchanged.")
        skip()
    if not isinstance(input_data, dict):
        # No session_id available outside a dict payload — the shared
        # "<no-session>" key still bounds this to one warning.
        if _should_warn_no_tools(""):
            _skip_with_message(
                "BeforeModel payload is not a JSON object. Passing through "
                "unchanged (warned once per session)."
            )
        skip()

    # 3. Extract tools array.
    tools, tools_declared = _extract_tools(input_data)
    if not tools:
        # A host that declares no tools for this turn (empty canonical array)
        # skips silently — that is a normal request. But a BeforeModel event
        # with no tools field at any known position means there is nothing
        # for schema compression to work on. That skip used to be silent, so
        # "0 schema records" could not be told apart from "hook never ran";
        # warn once per session instead.
        if not tools_declared and _should_warn_no_tools(
            str(input_data.get("session_id", ""))
        ):
            if not isinstance(input_data.get("llm_request"), dict):
                _skip_with_message(
                    "BeforeModel payload carries no llm_request object, so schema "
                    "compression cannot find tool declarations. Passing through "
                    "unchanged (warned once per session)."
                )
            else:
                _skip_with_message(
                    "BeforeModel event carries no tool declarations at "
                    "llm_request.config.tools or llm_request.tools, so schema "
                    "compression was skipped. If the host declares tools, its "
                    "event format may not match the hook. Passing through "
                    "unchanged (warned once per session)."
                )
        skip()

    # 4. Extract caller context
    session_id = input_data.get("session_id", "")
    tool_use_id = resolve_tool_call_id(_AGENT_ID, input_data)

    # 5. Compress schemas via the unified entry point (one subprocess).
    llm_request = input_data.get("llm_request", {})
    visible_context = dict(llm_request) if isinstance(llm_request, dict) else {}
    visible_context.pop("tools", None)
    config = visible_context.get("config")
    if isinstance(config, dict):
        visible_context["config"] = dict(config)
        visible_context["config"].pop("tools", None)
    request = build_before_model_request(
        tools,
        visible_context,
        _AGENT_ID,
        session_id=session_id,
        tool_use_id=tool_use_id,
    )
    response = run_compress(
        tokenless_bin, request, _COMPRESS_TIMEOUT, "before_model"
    )
    if response is None:
        warn("Schema compression subprocess failed. Passing through unchanged.")
        skip()
    compressed = response.get("tools")
    if not isinstance(compressed, list):
        warn(
            "Schema compression returned invalid JSON. Passing through unchanged."
        )
        skip()

    # 6. Build response at the canonical position.
    output = {
        "hookSpecificOutput": {
            "hookEventName": "BeforeModel",
            "llm_request": {
                "config": {
                    "tools": compressed,
                },
            },
        },
    }
    print(json.dumps(output))


if __name__ == "__main__":
    main()
