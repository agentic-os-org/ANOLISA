#!/usr/bin/env python3
"""Cosh hook script for skill-ledger.

Reads a cosh PreToolUse JSON from stdin, normalizes the legacy Cosh or
Cosh-NG skill invocation, resolves the skill directory from the skill context or
skill name, invokes ``agent-sec-cli skill-ledger show`` via subprocess, and
writes a cosh HookOutput JSON to stdout.

Hook point: **PreToolUse** — matcher: ``skill``

Input schema::

    {
        "session_id": "...",
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": { "skill": "<skill-name>" },
        "cwd": "/path/to/project"
    }

Legacy Cosh uses ``tool_input.skill``.  Cosh-NG uses
``tool_input.action``/``tool_input.name``; a missing or non-string action means
``invoke`` and ``action: "list"`` does not identify or execute a skill.

Output mapping:

    summary.message is null → { "decision": "allow" }
    policy "ask" (default)   → summary.message asks for confirmation
    policy "observe"         → summary.message only writes audit/debug stderr
    policy "warn"            → summary.message allows with visible reason
    policy "block"           → summary.message blocks execution

Optional Cosh settings.json configuration::

    {
      "hooks": {
        "PreToolUse": [{
          "matcher": "skill",
          "hooks": [{
            "type": "command",
            "name": "skill-ledger",
            "command": "python3 cosh-extension/hooks/skill_ledger_hook.py",
            "timeout": 10000
          }]
        }]
      }
    }

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` on $PATH.
"""

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

from hook_config import env_flag_enabled, env_hook_policy, normalize_hook_policy
from trace_context import with_trace_context

# -- constants ---------------------------------------------------------------

_TOOL_NAME = "skill"
_CHECK_TIMEOUT = 5  # seconds for the CLI show call
_INIT_TIMEOUT = 3  # seconds for key initialization

_DEFAULT_POLICY = "ask"
_HOOK_ENABLED = env_flag_enabled("SKILL_LEDGER_HOOK_ENABLED", True)
_MISSING = object()

# -- helpers -----------------------------------------------------------------


def _allow() -> str:
    """Return a permissive cosh HookOutput JSON string."""
    return json.dumps({"decision": "allow"})


def _allow_with_reason(reason: str) -> str:
    """Return an allow decision with a warning reason for display."""
    return json.dumps({"decision": "allow", "reason": reason}, ensure_ascii=False)


def _ask_with_reason(reason: str) -> str:
    """Return an ask decision with a confirmation reason for display."""
    return json.dumps({"decision": "ask", "reason": reason}, ensure_ascii=False)


def _block_with_reason(reason: str) -> str:
    """Return a block decision with a user-visible reason."""
    return json.dumps({"decision": "block", "reason": reason}, ensure_ascii=False)


def _debug(message: str) -> None:
    """Write debug-only hook details to stderr."""
    print(f"[skill-ledger debug] {message}", file=sys.stderr)


def _allow_or_warn(reason: str, _policy: str) -> str:
    """Fail open for pre-summary diagnostics without prompting the user."""
    _debug(reason)
    return _allow()


def _format_contract_error(detail: str, policy: str) -> str:
    """Map an invalid Host invocation contract to the configured policy."""
    reason = "\u26a0\ufe0f Skill check cannot bind the invoked identity: {}".format(
        detail
    )
    if policy == "observe":
        _debug(reason)
        return _allow()
    if policy == "warn":
        return _allow_with_reason(reason)
    if policy == "block":
        return _block_with_reason(reason)
    return _ask_with_reason(reason)


def _read_policy() -> str:
    """Return the configured Skill Ledger mode."""
    raw = os.environ.get("SKILL_LEDGER_MODE")
    policy = env_hook_policy("SKILL_LEDGER_MODE", _DEFAULT_POLICY)
    if "SKILL_LEDGER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        _debug("invalid SKILL_LEDGER_MODE; using ask")
    return policy


def _validate_identity(value: Any, field_name: str) -> tuple[str | None, str | None]:
    """Return an exact identity value or a Host contract error."""
    if value is _MISSING:
        return None, "{} is empty or missing".format(field_name)
    if not isinstance(value, str):
        return None, "{} must be a string".format(field_name)
    if not value.strip():
        return None, "{} is empty or missing".format(field_name)
    return value, None


def _normalize_skill_call(
    tool_input: Any,
) -> tuple[str | None, str | None, bool]:
    """Normalize Host input into ``(identity, error, skip)``.

    The presence of ``action`` or ``name`` selects the Cosh-NG profile.  The
    identity remains byte-for-byte equivalent to the Host value; whitespace is
    inspected only to reject empty values.
    """
    if not isinstance(tool_input, dict):
        return None, "tool_input must be an object", False

    is_cosh_ng = "action" in tool_input or "name" in tool_input
    if not is_cosh_ng:
        skill_name, error = _validate_identity(
            tool_input.get("skill", _MISSING), "skill"
        )
        return skill_name, error, False

    action = tool_input.get("action")
    if isinstance(action, str):
        if action == "list":
            return None, None, True
        if action != "invoke":
            return None, "unsupported Cosh-NG action {!r}".format(action), False

    skill_name, error = _validate_identity(tool_input.get("name", _MISSING), "name")
    if error is not None:
        return None, error, False

    if "skill" in tool_input:
        legacy_name, legacy_error = _validate_identity(tool_input.get("skill"), "skill")
        if legacy_error is not None:
            return None, legacy_error, False
        if legacy_name != skill_name:
            return (
                None,
                "Cosh-NG name and legacy skill identities conflict",
                False,
            )

    return skill_name, None, False


def _validate_skill_context(input_data: dict[str, Any], skill_name: str) -> str | None:
    """Validate a present context without letting it override Host identity."""
    if "skill_context" not in input_data:
        return None

    skill_context = input_data.get("skill_context")
    if not isinstance(skill_context, dict):
        return "skill_context must be an object"

    context_name, error = _validate_identity(
        skill_context.get("skill_name", _MISSING), "skill_context.skill_name"
    )
    if error is not None:
        return error

    _file_path, error = _validate_identity(
        skill_context.get("file_path", _MISSING), "skill_context.file_path"
    )
    if error is not None:
        return error

    if context_name != skill_name:
        return "skill_context.skill_name conflicts with the invoked identity"
    return None


def _supported_skill_bases(cwd: str) -> list[Path]:
    """Return project, legacy/raw user, and RPM/raw system skill roots."""
    data_home = os.environ.get("XDG_DATA_HOME", "")
    # Keep this standalone hook aligned with ANOLISA and the Ledger CLI.
    if not Path(data_home).is_absolute() or any(
        segment in (".", "..") for segment in data_home.split("/")
    ):
        data_home = str(Path.home() / ".local/share")
    return [
        Path(cwd) / ".copilot-shell" / "skills",
        Path.home() / ".copilot-shell" / "skills",
        Path(data_home) / "anolisa/skills",
        Path("/usr/share/anolisa/skills"),
        Path("/usr/local/share/anolisa/skills"),
    ]


def _resolve_supported_skill_bases(cwd: str, skill_name: str) -> list[Path]:
    """Resolve supported skill roots, skipping only roots that fail."""
    supported_bases: list[Path] = []
    for base in _supported_skill_bases(cwd):
        try:
            supported_bases.append(base.resolve())
        except (OSError, ValueError) as exc:
            _debug(
                "Skill '{}' check skipped for base '{}': failed to resolve: {}".format(
                    skill_name, base, exc
                )
            )
    return supported_bases


def _resolve_skill_dir_from_context(
    input_data: dict, cwd: str, skill_name: str
) -> tuple[str | None, bool]:
    """Resolve the skill dir from ``skill_context.file_path`` when available.

    The caller validates a present context before invoking this function.
    Returns ``(skill_dir, handled)``.  ``handled`` is True whenever a
    well-formed ``skill_context.file_path`` was present, even if the path is
    outside the supported project/user/system scope.  In that case the caller
    should fail open without falling back to name-based lookup, because the
    context identifies the actual skill that Cosh resolved.
    """
    skill_context = input_data.get("skill_context")
    if not isinstance(skill_context, dict):
        return None, False

    file_path = skill_context.get("file_path")
    if not isinstance(file_path, str) or not file_path.strip():
        return None, False

    try:
        skill_file = Path(file_path).expanduser().resolve()
    except (OSError, ValueError) as exc:
        _debug(
            "Skill '{}' check skipped: invalid skill_context.file_path '{}': {}".format(
                skill_name, file_path, exc
            )
        )
        return None, True

    supported_bases = _resolve_supported_skill_bases(cwd, skill_name)
    if not supported_bases:
        _debug(
            "Skill '{}' check skipped: no supported skill bases could be resolved".format(
                skill_name
            )
        )
        return None, True

    if not any(skill_file.is_relative_to(base) for base in supported_bases):
        _debug(
            "Skill '{}' at '{}' is outside current skill-ledger hook scope "
            "(project/user/system); check skipped".format(skill_name, skill_file)
        )
        return None, True

    if skill_file.name != "SKILL.md" or not skill_file.is_file():
        _debug(
            "Skill '{}' check skipped: skill_context.file_path '{}' does not "
            "point to an existing SKILL.md".format(skill_name, skill_file)
        )
        return None, True

    return str(skill_file.parent), True


def _resolve_skill_dir(skill_name: str, cwd: str) -> tuple[str | None, bool]:
    """Resolve a skill name to its on-disk directory.

    Searches the same project, legacy/raw user, and system roots as
    context-based resolution.

    Returns ``(path, traversal_detected)``:
    - ``(str, False)`` — resolved successfully.
    - ``(None, True)`` — path escapes the skills base (traversal attempt).
    - ``(None, False)`` — not found (remote or unknown skill).
    """
    traversal_detected = False
    bases = _supported_skill_bases(cwd)
    for base in bases:
        candidate = base / skill_name
        try:
            resolved_base = base.resolve()
            resolved = candidate.resolve()
        except (OSError, ValueError):
            continue
        if not resolved.is_relative_to(resolved_base):
            traversal_detected = True
            continue  # path-traversal attempt — skip this base
        if resolved.is_dir() and (resolved / "SKILL.md").is_file():
            return str(resolved), False

    return None, traversal_detected


def _keys_exist() -> bool:
    """Return True if both key.pub and key.enc exist."""
    xdg_data = os.environ.get("XDG_DATA_HOME", "")
    if not xdg_data:
        xdg_data = str(Path.home() / ".local" / "share")
    data_dir = Path(xdg_data) / "agent-sec" / "skill-ledger"
    return (data_dir / "key.pub").is_file() and (data_dir / "key.enc").is_file()


def _ensure_keys(input_data: dict[str, Any]) -> None:
    """Auto-initialize signing keys if missing (fire-and-forget)."""
    if _keys_exist():
        return
    try:
        cmd = with_trace_context(
            ["agent-sec-cli", "skill-ledger", "init", "--no-baseline"],
            input_data,
        )
        result = subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            text=True,
            timeout=_INIT_TIMEOUT,
        )
        if result.returncode != 0:
            _debug(
                "key init failed, exit_code={}, stderr={!r}".format(
                    result.returncode, result.stderr
                )
            )
    except Exception as exc:
        _debug("key init failed: {}".format(exc))


def _format_cosh(summary: dict, skill_name: str, policy: str) -> str:
    """Convert an exposure summary into a cosh HookOutput JSON string."""
    message = summary.get("message")
    if not isinstance(message, str) or not message.strip():
        return _allow()

    reason = f"\u26a0\ufe0f Skill '{skill_name}': {message}"
    if policy == "observe":
        _debug(reason)
        return _allow()
    if policy == "warn":
        return _allow_with_reason(reason)
    if policy == "block":
        return _block_with_reason(reason)
    return _ask_with_reason(reason)


# -- main --------------------------------------------------------------------


def main() -> None:
    """Entry point — read stdin, summarize skill exposure, write stdout."""
    if not _HOOK_ENABLED:
        print(_allow())
        return
    # 1. Read stdin JSON (PreToolUse event)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        print(_allow())
        return

    if not isinstance(input_data, dict):
        print(_allow())
        return

    # 2. Verify this is a skill tool call
    tool_name = input_data.get("tool_name", "")
    if tool_name != _TOOL_NAME:
        print(_allow())
        return

    tool_input = input_data.get("tool_input", {})
    skill_name, contract_error, skip = _normalize_skill_call(tool_input)
    if skip:
        print(_allow())
        return

    policy = _read_policy()
    if contract_error is not None or skill_name is None:
        detail = contract_error or "skill identity is empty or missing"
        print(_format_contract_error(detail, policy))
        return

    context_error = _validate_skill_context(input_data, skill_name)
    if context_error is not None:
        print(_format_contract_error(context_error, policy))
        return

    # 3. Resolve skill directory.  Prefer Cosh's resolved file path
    # when present so SKILL.md names may differ from directory names, but only
    # within the current project/user/system scope.
    cwd = input_data.get("cwd", os.environ.get("COPILOT_SHELL_PROJECT_DIR", "."))
    skill_dir, context_handled = _resolve_skill_dir_from_context(
        input_data, cwd, skill_name
    )
    if context_handled:
        if skill_dir is None:
            print(_allow())
            return
        traversal = False
    else:
        skill_dir, traversal = _resolve_skill_dir(skill_name, cwd)

    if traversal:
        reason = "\U0001f6a8 Skill '{}' rejected: path traversal detected".format(
            skill_name
        )
        print(_allow_or_warn(reason, policy))
        return
    if skill_dir is None:
        # Not found in any supported location (project/user/system) → fail-open
        reason = (
            "\u26a0\ufe0f Skill '{}' not found on disk \u2014 check skipped".format(
                skill_name
            )
        )
        print(_allow_or_warn(reason, policy))
        return

    # 4. Ensure signing keys exist (auto-init if missing)
    _ensure_keys(input_data)

    # 5. Call agent-sec-cli skill-ledger show <skill_dir>
    try:
        cmd = with_trace_context(
            ["agent-sec-cli", "skill-ledger", "show", skill_dir],
            input_data,
        )
        proc = subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            text=True,
            timeout=_CHECK_TIMEOUT,
        )
    except Exception:
        # Timeout or CLI not found → fail-open
        _debug("skill='{}' check failed before CLI output".format(skill_name))
        print(_allow())
        return

    # 6. Parse exposure summary and format output
    try:
        exposure_summary = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        _debug(
            "skill='{}' invalid CLI JSON, exit_code={}, stderr={!r}".format(
                skill_name, proc.returncode, proc.stderr
            )
        )
        print(_allow())
        return

    print(_format_cosh(exposure_summary, skill_name, policy))


if __name__ == "__main__":
    main()
