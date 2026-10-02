#!/usr/bin/env python3
"""Enforce Skill Ledger exposure decisions for managed Qwen Code skills.

The hook is intentionally fail-open. It protects model-triggered ``skill``
tool calls for managed project and user Qwen skills, while unsupported,
unmanaged, ambiguous, or inaccessible sources continue without a Qwen
permission override.
"""

import json
import os
import re
import stat
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from hook_config import env_flag_enabled, env_hook_policy, normalize_hook_policy
from trace_context import trace_context, with_trace_context

_TOOL_NAME = "skill"
_CHECK_TIMEOUT_SECONDS = 5
_INIT_TIMEOUT_SECONDS = 3
_DEFAULT_POLICY = "ask"
_HOOK_ENABLED = env_flag_enabled("SKILL_LEDGER_HOOK_ENABLED", True)
_LEDGER_STATUSES = frozenset({"pass", "none", "drifted", "warn", "deny", "tampered"})
_MAX_FRONTMATTER_LINES = 128
_ENV_VAR_PATTERN = re.compile(r"\$(?:(\w+)|{([^}]+)})", flags=re.ASCII)


@dataclass(frozen=True)
class _ResolvedSkill:
    """One safely resolved disk Skill and its model-visibility metadata."""

    directory: Path
    disable_model_invocation: bool


def _noop() -> str:
    """Return an empty Qwen HookOutput without overriding host permissions."""
    return json.dumps({})


def _system_message(reason: str) -> str:
    """Return a visible warning without approving the tool call."""
    return json.dumps({"systemMessage": reason}, ensure_ascii=False)


def _permission_decision(decision: str, reason: str) -> str:
    """Return an official Qwen PreToolUse permission decision."""
    return json.dumps(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": decision,
                "permissionDecisionReason": reason,
            }
        },
        ensure_ascii=False,
    )


def _diagnostic(
    code: str,
    input_data: dict[str, Any],
    *,
    skill_name: str | None = None,
    status: str | None = None,
    detail: str | None = None,
) -> None:
    """Write one structured, session-correlated diagnostic to stderr."""
    payload: dict[str, str] = {
        "event": "skill_ledger_hook",
        "code": code,
        **trace_context(input_data),
    }
    if skill_name:
        payload["skill_name"] = skill_name
    if status:
        payload["status"] = status
    if detail:
        payload["detail"] = detail
    print(
        "qwen-skill-ledger-hook: "
        + json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
        file=sys.stderr,
    )


def _read_policy(input_data: dict[str, Any]) -> str:
    """Return the configured Skill Ledger mode, defaulting invalid values to ask."""
    raw = os.environ.get("SKILL_LEDGER_MODE")
    policy = env_hook_policy("SKILL_LEDGER_MODE", _DEFAULT_POLICY)
    if "SKILL_LEDGER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        _diagnostic(
            "invalid_policy", input_data, detail="invalid SKILL_LEDGER_MODE; using ask"
        )
    return policy


def _valid_skill_name(skill_name: str) -> bool:
    """Reject path-like values without using them for filesystem lookup."""
    return bool(
        skill_name
        and skill_name not in {".", ".."}
        and "/" not in skill_name
        and "\\" not in skill_name
        and "\x00" not in skill_name
        and not Path(skill_name).is_absolute()
    )


def _frontmatter_scalar(value: str) -> str | None:
    """Parse the plain or quoted scalar forms accepted for a Skill name."""
    value = value.strip()
    if not value:
        return None
    if value.startswith('"') and value.endswith('"'):
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError:
            return None
        return decoded.strip() if isinstance(decoded, str) and decoded.strip() else None
    if value.startswith("'") and value.endswith("'"):
        decoded = value[1:-1].replace("''", "'").strip()
        return decoded or None
    if " #" in value:
        value = value.split(" #", 1)[0].rstrip()
    return value or None


def _frontmatter_disables_model(value: str) -> bool | None:
    """Parse Qwen's boolean-or-literal-"true" model-invocation flag."""
    raw = value.strip()
    if not raw:
        return False
    if raw.startswith('"'):
        if not raw.endswith('"'):
            return None
        try:
            decoded = json.loads(raw)
        except json.JSONDecodeError:
            return None
        return decoded == "true"
    if raw.startswith("'"):
        if not raw.endswith("'"):
            return None
        return raw[1:-1].replace("''", "'") == "true"
    if " #" in raw:
        raw = raw.split(" #", 1)[0].rstrip()
    return raw.lower() == "true"


def _skill_frontmatter_metadata(skill_file: Path) -> tuple[str, bool] | None:
    """Read bounded metadata needed to identify one model-visible Skill."""
    try:
        with skill_file.open(encoding="utf-8-sig") as stream:
            if stream.readline().strip() != "---":
                return None
            name: str | None = None
            description_present = False
            disable_model_invocation = False
            for _ in range(_MAX_FRONTMATTER_LINES):
                line = stream.readline()
                if not line:
                    return None
                stripped = line.strip()
                if stripped == "---":
                    if name is None or not description_present:
                        return None
                    return name, disable_model_invocation
                key, separator, value = line.partition(":")
                key = key.strip()
                if separator and key == "name":
                    name = _frontmatter_scalar(value)
                elif separator and key == "description":
                    description_present = _frontmatter_scalar(value) is not None
                elif separator and key == "disable-model-invocation":
                    parsed = _frontmatter_disables_model(value)
                    if parsed is None:
                        return None
                    disable_model_invocation = parsed
    except (OSError, UnicodeError):
        return None
    return None


def _qwen_home(cwd: str) -> Path:
    """Resolve QWEN_HOME with the path semantics used by Qwen Code."""
    configured = os.environ.get("QWEN_HOME")
    if not configured:
        return Path.home() / ".qwen"

    resolved = configured
    if resolved == "~" or resolved.startswith("~/") or resolved.startswith("~\\"):
        suffix = resolved[2:] if resolved != "~" else ""
        segments = [segment for segment in re.split(r"[/\\]+", suffix) if segment]
        return Path.home().joinpath(*segments)

    configured_path = Path(resolved)
    if not configured_path.is_absolute():
        configured_path = Path(cwd) / configured_path
    return configured_path.resolve(strict=False)


def _supported_skill_bases(cwd: str) -> list[Path]:
    """Return project then user Qwen skill roots in runtime precedence order."""
    return [Path(cwd) / ".qwen" / "skills", _qwen_home(cwd) / "skills"]


def _candidate_skill_dirs(
    base: Path,
    skill_name: str,
    input_data: dict[str, Any],
) -> tuple[list[_ResolvedSkill], bool]:
    """Return matching candidates and whether the root itself was inaccessible."""
    try:
        if not base.exists():
            return [], False
        resolved_base = base.resolve(strict=True)
        if not resolved_base.is_dir():
            _diagnostic(
                "invalid_skill_root",
                input_data,
                skill_name=skill_name,
                detail=str(base),
            )
            return [], True
        entries = sorted(base.iterdir(), key=lambda path: path.name)
    except (OSError, ValueError) as exc:
        _diagnostic(
            "inaccessible_skill_root",
            input_data,
            skill_name=skill_name,
            detail=f"{base}: {type(exc).__name__}",
        )
        return [], True

    matches: list[_ResolvedSkill] = []
    for entry in entries:
        try:
            entry.lstat()
            resolved_dir = entry.resolve(strict=True)
            if not resolved_dir.is_relative_to(resolved_base):
                _diagnostic(
                    "symlink_outside_skill_root",
                    input_data,
                    skill_name=skill_name,
                    detail=str(entry),
                )
                continue
            if not stat.S_ISDIR(resolved_dir.stat().st_mode):
                continue

            skill_file = (resolved_dir / "SKILL.md").resolve(strict=True)
            if not skill_file.is_relative_to(resolved_base):
                _diagnostic(
                    "skill_file_outside_skill_root",
                    input_data,
                    skill_name=skill_name,
                    detail=str(entry),
                )
                continue
            if not stat.S_ISREG(skill_file.stat().st_mode):
                continue
        except (OSError, RuntimeError, ValueError) as exc:
            _diagnostic(
                "invalid_skill_candidate",
                input_data,
                skill_name=skill_name,
                detail=f"{entry}: {type(exc).__name__}",
            )
            continue

        metadata = _skill_frontmatter_metadata(skill_file)
        if metadata is not None and metadata[0] == skill_name:
            matches.append(
                _ResolvedSkill(
                    directory=resolved_dir,
                    disable_model_invocation=metadata[1],
                )
            )
    return matches, False


def _resolve_skill_dir(
    skill_name: str,
    cwd: str,
    input_data: dict[str, Any],
) -> _ResolvedSkill | None:
    """Resolve one supported Qwen skill without guessing ambiguous sources."""
    if not _valid_skill_name(skill_name):
        _diagnostic("invalid_skill_name", input_data, skill_name=skill_name)
        return None

    try:
        skill_bases = _supported_skill_bases(cwd)
    except (OSError, RuntimeError, ValueError) as exc:
        _diagnostic(
            "invalid_qwen_home",
            input_data,
            skill_name=skill_name,
            detail=type(exc).__name__,
        )
        return None

    for base in skill_bases:
        candidates, root_failed = _candidate_skill_dirs(base, skill_name, input_data)
        if root_failed:
            return None
        if len(candidates) > 1:
            _diagnostic(
                "ambiguous_same_level",
                input_data,
                skill_name=skill_name,
                detail=str(base),
            )
            return None
        if candidates:
            return candidates[0]

    _diagnostic(
        "unsupported_or_unresolved",
        input_data,
        skill_name=skill_name,
    )
    return None


def _strip_json_comments(content: str) -> str:
    """Remove JSON comments without treating comment markers in strings as syntax."""
    output: list[str] = []
    index = 0
    in_string = False
    escaped = False
    while index < len(content):
        char = content[index]
        if in_string:
            output.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            index += 1
            continue

        if char == '"':
            in_string = True
            output.append(char)
            index += 1
            continue

        next_char = content[index + 1] if index + 1 < len(content) else ""
        if char == "/" and next_char == "/":
            output.extend((" ", " "))
            index += 2
            while index < len(content) and content[index] not in "\r\n":
                output.append(" ")
                index += 1
            continue
        if char == "/" and next_char == "*":
            output.extend((" ", " "))
            index += 2
            while index < len(content):
                if (
                    content[index] == "*"
                    and index + 1 < len(content)
                    and content[index + 1] == "/"
                ):
                    output.extend((" ", " "))
                    index += 2
                    break
                output.append(content[index] if content[index] in "\r\n" else " ")
                index += 1
            else:
                raise ValueError("unterminated block comment")
            continue

        output.append(char)
        index += 1
    return "".join(output)


def _resolve_settings_path(value: str, cwd: str) -> Path:
    """Resolve a Qwen system settings override against the hook working dir."""
    path = Path(value)
    return path if path.is_absolute() else (Path(cwd) / path).resolve(strict=False)


def _system_settings_path(cwd: str) -> Path:
    """Return Qwen's platform-specific system settings path."""
    configured = os.environ.get("QWEN_CODE_SYSTEM_SETTINGS_PATH")
    if configured:
        return _resolve_settings_path(configured, cwd)
    if sys.platform == "darwin":
        return Path("/Library/Application Support/QwenCode/settings.json")
    if sys.platform == "win32":
        return Path(r"C:\ProgramData\qwen-code\settings.json")
    return Path("/etc/qwen-code/settings.json")


def _system_defaults_path(cwd: str, system_settings: Path) -> Path:
    """Return Qwen's system-defaults path, including its environment override."""
    configured = os.environ.get("QWEN_CODE_SYSTEM_DEFAULTS_PATH")
    if configured:
        return _resolve_settings_path(configured, cwd)
    return system_settings.parent / "system-defaults.json"


def _workspace_settings_active(cwd: str) -> bool:
    """Match Qwen's rule that omits workspace settings in the home directory."""
    home = Path.home().resolve(strict=True)
    workspace = Path(cwd).resolve(strict=False)
    try:
        workspace = workspace.resolve(strict=True)
    except OSError:
        pass
    return workspace != home


def _settings_paths(cwd: str) -> list[Path]:
    """Return Qwen settings scopes in their documented merge order."""
    system_settings = _system_settings_path(cwd)
    paths = [
        _system_defaults_path(cwd, system_settings),
        _qwen_home(cwd) / "settings.json",
    ]
    if _workspace_settings_active(cwd):
        paths.append(Path(cwd) / ".qwen" / "settings.json")
    paths.append(system_settings)
    return paths


def _expand_env_vars(value: str) -> str:
    """Expand Qwen-style $VAR and ${VAR} references from the hook environment."""

    def replace(match: re.Match[str]) -> str:
        variable = match.group(1) or match.group(2)
        return os.environ.get(variable, match.group(0))

    return _ENV_VAR_PATTERN.sub(replace, value)


def _reject_json_constant(value: str) -> None:
    """Reject non-standard constants accepted by Python but not JSON.parse."""
    raise ValueError(f"invalid JSON constant: {value}")


def _read_disabled_skill_names(
    cwd: str,
    input_data: dict[str, Any],
    skill_name: str,
) -> frozenset[str] | None:
    """Read only Qwen's UNION-merged skills.disabled setting."""
    try:
        settings_paths = _settings_paths(cwd)
    except (OSError, RuntimeError, ValueError) as exc:
        _diagnostic(
            "skill_visibility_unknown",
            input_data,
            skill_name=skill_name,
            detail=f"settings_paths:{type(exc).__name__}",
        )
        return None

    disabled: set[str] = set()
    for settings_path in settings_paths:
        try:
            settings_mode = settings_path.stat().st_mode
        except FileNotFoundError:
            continue
        except OSError as exc:
            _diagnostic(
                "skill_visibility_unknown",
                input_data,
                skill_name=skill_name,
                detail=f"{settings_path}:{type(exc).__name__}",
            )
            return None

        try:
            if not stat.S_ISREG(settings_mode):
                raise OSError("settings path is not a regular file")
            content = settings_path.read_text(encoding="utf-8")
            settings = json.loads(
                _strip_json_comments(content),
                parse_constant=_reject_json_constant,
            )
        except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as exc:
            _diagnostic(
                "skill_visibility_unknown",
                input_data,
                skill_name=skill_name,
                detail=f"{settings_path}:{type(exc).__name__}",
            )
            return None

        if not isinstance(settings, dict):
            _diagnostic(
                "skill_visibility_unknown",
                input_data,
                skill_name=skill_name,
                detail=f"{settings_path}:invalid_root",
            )
            return None
        skills = settings.get("skills")
        if not isinstance(skills, dict):
            continue
        raw_names = skills.get("disabled")
        if not isinstance(raw_names, list):
            continue
        disabled.update(
            normalized
            for raw_name in raw_names
            if isinstance(raw_name, str)
            if (normalized := _expand_env_vars(raw_name).strip().lower())
        )
    return frozenset(disabled)


def _is_model_invocable(
    skill: _ResolvedSkill,
    skill_name: str,
    cwd: str,
    input_data: dict[str, Any],
) -> bool:
    """Return whether Qwen can dispatch the tool call to this disk Skill."""
    if skill.disable_model_invocation:
        _diagnostic("model_invocation_disabled", input_data, skill_name=skill_name)
        return False

    disabled_names = _read_disabled_skill_names(cwd, input_data, skill_name)
    if disabled_names is None:
        return False
    if skill_name.lower() in disabled_names:
        _diagnostic("skill_disabled_by_settings", input_data, skill_name=skill_name)
        return False
    return True


def _keys_exist() -> bool:
    """Return whether the Skill Ledger public and private key files exist."""
    xdg_data = os.environ.get("XDG_DATA_HOME")
    data_root = Path(xdg_data) if xdg_data else Path.home() / ".local" / "share"
    ledger_dir = data_root / "agent-sec" / "skill-ledger"
    return (ledger_dir / "key.pub").is_file() and (ledger_dir / "key.enc").is_file()


def _ensure_keys(input_data: dict[str, Any], skill_name: str) -> None:
    """Best-effort initialize Skill Ledger keys without baselining skills."""
    if _keys_exist():
        return
    command = with_trace_context(
        ["agent-sec-cli", "skill-ledger", "init", "--no-baseline"],
        input_data,
    )
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            check=False,
            text=True,
            timeout=_INIT_TIMEOUT_SECONDS,
        )
    except Exception as exc:
        _diagnostic(
            "key_init_failed",
            input_data,
            skill_name=skill_name,
            detail=type(exc).__name__,
        )
        return
    if result.returncode != 0:
        _diagnostic(
            "key_init_failed",
            input_data,
            skill_name=skill_name,
            detail=f"exit_code={result.returncode}",
        )


def _show_skill(
    skill_dir: Path,
    input_data: dict[str, Any],
    skill_name: str,
) -> dict[str, Any] | None:
    """Return a validated Skill Ledger show response, or None on failure."""
    command = with_trace_context(
        ["agent-sec-cli", "skill-ledger", "show", str(skill_dir)],
        input_data,
    )
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            check=False,
            text=True,
            timeout=_CHECK_TIMEOUT_SECONDS,
        )
    except Exception as exc:
        _diagnostic(
            "show_failed",
            input_data,
            skill_name=skill_name,
            detail=type(exc).__name__,
        )
        return None
    if result.returncode != 0:
        _diagnostic(
            "show_failed",
            input_data,
            skill_name=skill_name,
            detail=f"exit_code={result.returncode}",
        )
        return None
    try:
        summary = json.loads(result.stdout)
    except (json.JSONDecodeError, ValueError):
        _diagnostic("invalid_show_json", input_data, skill_name=skill_name)
        return None
    if not isinstance(summary, dict):
        _diagnostic("invalid_show_result", input_data, skill_name=skill_name)
        return None
    return summary


def _format_qwen(
    summary: dict[str, Any],
    skill_name: str,
    policy: str,
    input_data: dict[str, Any],
) -> str:
    """Map a managed Skill Ledger exposure message to Qwen HookOutput."""
    # ``skill-ledger show`` marks only unmanaged results explicitly. Managed
    # results omit the field, while accepting ``True`` keeps the hook compatible
    # with callers that already provide an explicit marker.
    managed = summary.get("managed")
    if managed is False:
        _diagnostic("unmanaged", input_data, skill_name=skill_name)
        return _noop()
    if managed is not None and managed is not True:
        _diagnostic("invalid_managed", input_data, skill_name=skill_name)
        return _noop()

    status_value = summary.get("latestStatus")
    if not isinstance(status_value, str) or status_value not in _LEDGER_STATUSES:
        _diagnostic("unknown_status", input_data, skill_name=skill_name)
        return _noop()

    if "message" not in summary:
        _diagnostic(
            "missing_exposure_message",
            input_data,
            skill_name=skill_name,
            status=status_value,
        )
        return _noop()
    message = summary["message"]
    if message is None or (isinstance(message, str) and not message.strip()):
        return _noop()
    if not isinstance(message, str):
        _diagnostic(
            "invalid_exposure_message",
            input_data,
            skill_name=skill_name,
            status=status_value,
        )
        return _noop()

    reason = f"Skill Ledger [{status_value}] for '{skill_name}': {message.strip()}"
    if policy in {"debug", "observe"}:
        _diagnostic(
            "exposure_warning",
            input_data,
            skill_name=skill_name,
            status=status_value,
            detail=message.strip(),
        )
        return _noop()
    if policy == "warn":
        return _system_message(reason)
    if policy == "block":
        return _permission_decision("deny", reason)
    return _permission_decision("ask", reason)


def main() -> None:
    """Read one Qwen HookInput and emit one fail-open HookOutput."""
    if not _HOOK_ENABLED:
        print(_noop())
        return
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        print(_noop())
        return
    if not isinstance(input_data, dict):
        print(_noop())
        return

    try:
        if input_data.get("hook_event_name") != "PreToolUse":
            print(_noop())
            return
        if input_data.get("tool_name") != _TOOL_NAME:
            print(_noop())
            return

        tool_input = input_data.get("tool_input")
        if not isinstance(tool_input, dict):
            _diagnostic("invalid_tool_input", input_data)
            print(_noop())
            return
        skill_name_value = tool_input.get("skill")
        if not isinstance(skill_name_value, str) or not skill_name_value.strip():
            _diagnostic("invalid_skill_name", input_data)
            print(_noop())
            return
        skill_name = skill_name_value.strip()

        cwd = input_data.get("cwd")
        if not isinstance(cwd, str) or not cwd.strip():
            _diagnostic("invalid_cwd", input_data, skill_name=skill_name)
            print(_noop())
            return

        skill = _resolve_skill_dir(skill_name, cwd, input_data)
        if skill is None:
            print(_noop())
            return
        if not _is_model_invocable(skill, skill_name, cwd, input_data):
            print(_noop())
            return

        policy = _read_policy(input_data)
        _ensure_keys(input_data, skill_name)
        summary = _show_skill(skill.directory, input_data, skill_name)
        if summary is None:
            print(_noop())
            return
        print(_format_qwen(summary, skill_name, policy, input_data))
    except Exception as exc:
        _diagnostic(
            "unexpected_error",
            input_data,
            detail=type(exc).__name__,
        )
        print(_noop())


if __name__ == "__main__":
    main()
