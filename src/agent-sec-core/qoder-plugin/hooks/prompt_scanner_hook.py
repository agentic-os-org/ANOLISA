#!/usr/bin/env python3
"""Qoder UserPromptSubmit hook for prompt injection and jailbreak detection."""

import json
import os
import subprocess
from typing import Any

from qoder_hook_common import (
    deny_output,
    dumps_hook_output,
    env_flag_enabled,
    load_hook_input,
    with_trace_context,
)

_HOOK_ENABLED = env_flag_enabled("PROMPT_SCANNER_HOOK_ENABLED", True)
_MODE = os.environ.get("PROMPT_SCANNER_MODE", "observe").strip().lower()
_VALID_MODES = {"observe", "deny"}
try:
    _TIMEOUT = int(os.environ.get("PROMPT_SCANNER_TIMEOUT", "10"))
except (TypeError, ValueError):
    _TIMEOUT = 10

_DEFAULT_SCAN_MODE = (
    os.environ.get("PROMPT_SCANNER_SCAN_MODE", "standard").strip().lower()
)
if _DEFAULT_SCAN_MODE not in {"fast", "standard", "strict"}:
    _DEFAULT_SCAN_MODE = "standard"

_DEFAULT_SOURCE = "user_input"


def _safe_string(value: Any) -> str:
    """Return value when it is a string, otherwise an empty string."""
    return value if isinstance(value, str) else ""


def _format_notice(scan_result: dict[str, Any]) -> str:
    """Build a user-visible prompt-scanner notice from the scan result."""
    threat_type = _safe_string(scan_result.get("threat_type")) or "unknown"
    risk_level = _safe_string(scan_result.get("risk_level")) or "unknown"
    confidence = scan_result.get("confidence")

    parts = [
        "[prompt-scanner] 检测到提示词安全风险",
        f"  攻击类型: {threat_type}",
        f"  风险等级: {risk_level}",
    ]
    if confidence is not None:
        try:
            parts.append(f"  置信度: {float(confidence) * 100:.1f}%")
        except (TypeError, ValueError):
            pass
    parts.append("该提示词已被安全策略阻止，请修改后重试。")
    return "\n".join(parts)


def _warn_output(notice: str) -> str:
    """Return an allow decision with a user-visible system message."""
    return dumps_hook_output({"decision": "allow", "systemMessage": notice})


def _invalid_mode_output() -> str:
    """Return a visible fail-open warning for an invalid scanner mode."""
    configured_mode = _safe_string(_MODE) or "<empty>"
    notice = (
        f"[prompt-scanner] Invalid PROMPT_SCANNER_MODE {configured_mode!r}; expected "
        "'observe' or 'deny'. Falling back to observe mode; execution will continue."
    )
    return _warn_output(notice)


def _format_decision(scan_result: dict[str, Any]) -> str | None:
    """Map a scan-prompt verdict to Qoder hook output."""
    verdict = _safe_string(scan_result.get("verdict")) or "pass"

    if verdict in {"pass", "error"}:
        return None

    if verdict not in {"warn", "deny"}:
        return None

    if _MODE != "deny":
        return None

    notice = _format_notice(scan_result)
    return deny_output(notice)


def _scan_prompt(input_data: dict[str, Any], prompt_text: str) -> dict[str, Any] | None:
    """Run agent-sec-cli scan-prompt and parse its JSON response."""
    args = [
        "agent-sec-cli",
        "scan-prompt",
        "--mode",
        _DEFAULT_SCAN_MODE,
        "--format",
        "json",
        "--source",
        _DEFAULT_SOURCE,
    ]

    try:
        proc = subprocess.run(
            with_trace_context(args, input_data),
            capture_output=True,
            check=False,
            input=prompt_text,
            text=True,
            timeout=_TIMEOUT,
        )
    except (OSError, subprocess.SubprocessError, TimeoutError):
        return None

    if proc.returncode != 0:
        return None

    try:
        scan_result = json.loads(proc.stdout)
    except (ValueError, TypeError):
        return None
    return scan_result if isinstance(scan_result, dict) else None


def main() -> None:
    """Run the Qoder prompt scanner hook."""
    if not _HOOK_ENABLED:
        return

    input_data = load_hook_input()
    if input_data is None:
        return

    event_name = _safe_string(input_data.get("hook_event_name"))
    if event_name != "UserPromptSubmit":
        return

    prompt_text = _safe_string(input_data.get("prompt"))
    if not prompt_text.strip():
        return

    scan_result = _scan_prompt(input_data, prompt_text)
    if _MODE not in _VALID_MODES:
        print(_invalid_mode_output())
        return
    if scan_result is None:
        return

    output = _format_decision(scan_result)
    if output:
        print(output)


if __name__ == "__main__":
    main()
