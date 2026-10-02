#!/usr/bin/env python3
"""Qwen Code PreToolUse hook for command code scanning."""

import json
import os
import subprocess
import sys
from typing import Any

from hook_config import env_flag_enabled, normalize_hook_policy
from trace_context import with_trace_context

_HOOK_ENABLED = env_flag_enabled("CODE_SCANNER_HOOK_ENABLED", True)
_TOOL_NAME = "run_shell_command"
_LANGUAGE = "bash"
_MAX_FINDINGS_DISPLAY = 5
_MAX_TEXT_CHARS = 120


def _diagnostic(message: str) -> None:
    print(f"[code-scanner] {' '.join(message.split())}", file=sys.stderr)


def _read_mode() -> str:
    raw = os.environ.get("CODE_SCANNER_MODE")
    mode = normalize_hook_policy(raw, "")
    if raw is not None and mode not in {"observe", "ask", "block"}:
        _diagnostic(
            f"invalid or unsupported CODE_SCANNER_MODE={raw[:32]!r}; using observe"
        )
        return "observe"
    return mode or "observe"


_MODE = _read_mode()
try:
    _TIMEOUT = int(os.environ.get("CODE_SCANNER_TIMEOUT", "10"))
except (TypeError, ValueError):
    _TIMEOUT = 10


def _json_output(payload: dict[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, separators=(",", ":"))


def _noop() -> str:
    return _json_output({})


def _compact_text(value: str, limit: int = _MAX_TEXT_CHARS) -> str:
    value = " ".join(value.split())
    if len(value) <= limit:
        return value
    return value[: limit - 1] + "..."


def _load_input() -> dict[str, Any] | None:
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        return None
    return input_data if isinstance(input_data, dict) else None


def _extract_command(input_data: dict[str, Any]) -> str | None:
    if input_data.get("hook_event_name") != "PreToolUse":
        return None
    if input_data.get("tool_name") != _TOOL_NAME:
        return None

    tool_input = input_data.get("tool_input")
    if isinstance(tool_input, str) and tool_input.strip().startswith(("{", "[")):
        try:
            tool_input = json.loads(tool_input)
        except (json.JSONDecodeError, ValueError):
            return None
    if not isinstance(tool_input, dict):
        return None

    command = tool_input.get("command")
    if not isinstance(command, str) or not command.strip():
        return None
    return command


def _scan_code(input_data: dict[str, Any], command: str) -> dict[str, Any] | None:
    try:
        proc = subprocess.run(
            with_trace_context(
                [
                    "agent-sec-cli",
                    "scan-code",
                    "--code",
                    command,
                    "--language",
                    _LANGUAGE,
                ],
                input_data,
            ),
            capture_output=True,
            check=False,
            text=True,
            timeout=_TIMEOUT,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None

    try:
        scan = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        return None
    return scan if isinstance(scan, dict) else None


def _format_message(scan: dict[str, Any], final_message: str) -> str:
    findings = [item for item in scan.get("findings", []) if isinstance(item, dict)]
    lines = [
        f"[code-scanner] Detected {len(findings)} risk finding(s) in this shell command."
    ]
    for finding in findings[:_MAX_FINDINGS_DISPLAY]:
        rule_id = finding.get("rule_id") or "unknown"
        desc = finding.get("desc_zh") or finding.get("desc_en") or ""
        line = f"- {rule_id}"
        if isinstance(desc, str) and desc:
            line += f": {_compact_text(desc)}"
        lines.append(line)
    remaining = len(findings) - _MAX_FINDINGS_DISPLAY
    if remaining > 0:
        lines.append(f"- ... and {remaining} more finding(s)")
    lines.append(final_message)
    return "\n".join(lines)


def _decision(decision: str, reason: str) -> str:
    return _json_output(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": decision,
                "permissionDecisionReason": reason,
            }
        }
    )


def _format_decision(scan: dict[str, Any]) -> str | None:
    verdict = scan.get("verdict", "pass")
    findings = scan.get("findings", [])
    if verdict in {"pass", "error"} or not findings:
        return None
    if verdict not in {"warn", "deny"}:
        return None
    if _MODE == "ask":
        return _decision(
            "ask",
            _format_message(scan, "Review this command before execution."),
        )
    if _MODE == "block":
        return _decision(
            "deny",
            _format_message(scan, "This command was denied before execution."),
        )
    return None


def main() -> None:
    if not _HOOK_ENABLED:
        print(_noop())
        return

    input_data = _load_input()
    if input_data is None:
        print(_noop())
        return

    command = _extract_command(input_data)
    if command is None:
        print(_noop())
        return

    scan = _scan_code(input_data, command)
    if scan is None:
        print(_noop())
        return

    print(_format_decision(scan) or _noop())


if __name__ == "__main__":
    main()
