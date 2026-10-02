#!/usr/bin/env python3
"""Qwen Code UserPromptSubmit hook for prompt-scanner.

Reads a Qwen Code UserPromptSubmit JSON from stdin, extracts the user prompt,
invokes ``agent-sec-cli scan-prompt`` via subprocess, and writes a Qwen Code
HookOutput JSON to stdout.

Modes (controlled by ``PROMPT_SCANNER_MODE`` env var, default: ``observe``):

- ``observe``: silent pass-through; the scan result is still sent to
  ``agent-sec-cli`` for audit purposes, but the prompt is never blocked.
- ``deny``: block the prompt when prompt injection is detected.

``PROMPT_SCANNER_TIMEOUT`` controls the inner ``agent-sec-cli`` timeout in
seconds.

Scan mode (controlled by ``PROMPT_SCANNER_SCAN_MODE`` env var, default:
``standard``):

- ``fast``: lightweight heuristics, lower latency.
- ``standard``: balanced detection (default).
- ``strict``: not implemented yet; currently behaves the same as standard.

Usage::

    python3 prompt_scanner_hook.py          # reads stdin, writes stdout

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` binary on ``$PATH``.
"""

import json
import os
import subprocess
import sys
from typing import Any

from hook_config import env_flag_enabled
from trace_context import with_trace_context

# -- config ----------------------------------------------------------------

_HOOK_ENABLED = env_flag_enabled("PROMPT_SCANNER_HOOK_ENABLED", True)
_MODE = os.environ.get("PROMPT_SCANNER_MODE", "observe").strip().lower()
_VALID_MODES = {"observe", "deny"}
try:
    _TIMEOUT = int(os.environ.get("PROMPT_SCANNER_TIMEOUT", "10"))
except (TypeError, ValueError):
    _TIMEOUT = 10
_RAW_SCAN_MODE = os.environ.get("PROMPT_SCANNER_SCAN_MODE", "standard").strip().lower()
_DEFAULT_SCAN_MODE = _RAW_SCAN_MODE
if _DEFAULT_SCAN_MODE not in {"fast", "standard", "strict"}:
    _DEFAULT_SCAN_MODE = "standard"
_DEFAULT_SOURCE = "user_input"


# -- helpers ---------------------------------------------------------------


def _warn_invalid_scan_mode() -> None:
    """Report a PROMPT_SCANNER_SCAN_MODE value that silently fell back.

    Mirrors the pii-checker and skill-ledger hooks: only the misconfiguration
    is surfaced, so a correctly configured hook stays silent.
    """
    if (
        "PROMPT_SCANNER_SCAN_MODE" in os.environ
        and _RAW_SCAN_MODE != _DEFAULT_SCAN_MODE
    ):
        print(
            f"[prompt-scanner] invalid PROMPT_SCANNER_SCAN_MODE {_RAW_SCAN_MODE!r}; "
            f"using {_DEFAULT_SCAN_MODE!r}",
            file=sys.stderr,
        )


def _noop() -> str:
    """Return an empty Qwen Code HookOutput JSON string."""
    return json.dumps({})


def _build_reason(scan_result: dict[str, Any]) -> str:
    """Build a detailed reason string from the scan result."""
    threat_type = scan_result.get("threat_type", "")
    risk_level = scan_result.get("risk_level", "unknown")
    confidence = scan_result.get("confidence")

    lines = [
        "[prompt-scanner] 检测到提示词安全风险",
        f"  攻击类型: {threat_type or 'unknown'}",
        f"  风险等级: {risk_level}",
    ]
    if confidence is not None:
        try:
            lines.append(f"  置信度: {float(confidence) * 100:.1f}%")
        except (TypeError, ValueError):
            pass
    lines.append("该提示词已被安全策略阻止，请修改后重试。")
    return "\n".join(lines)


def _block(scan_result: dict[str, Any]) -> None:
    """Output a blocking decision to reject the prompt."""
    reason = _build_reason(scan_result)
    print(json.dumps({"decision": "deny", "reason": reason}, ensure_ascii=False))


def _invalid_mode_output() -> str:
    """Return a visible fail-open warning for an invalid scanner mode."""
    configured_mode = _MODE[:32] or "<empty>"
    return json.dumps(
        {
            "decision": "allow",
            "systemMessage": (
                f"[prompt-scanner] Invalid PROMPT_SCANNER_MODE {configured_mode!r}; expected "
                "'observe' or 'deny'. Falling back to observe mode; execution will continue."
            ),
        },
        ensure_ascii=False,
    )


# -- main ------------------------------------------------------------------


def main() -> None:
    if not _HOOK_ENABLED:
        print(_noop())
        return

    _warn_invalid_scan_mode()

    # 1. Read stdin JSON (fail-open)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        print(_noop())
        return

    if not isinstance(input_data, dict):
        print(_noop())
        return

    # 2. Extract user prompt text
    prompt_text = input_data.get("prompt", "")
    if not isinstance(prompt_text, str) or not prompt_text.strip():
        print(_noop())
        return

    # 3. Validate mode before invoking the CLI
    if _MODE not in _VALID_MODES:
        print(_invalid_mode_output())
        return

    # 4. Call agent-sec-cli scan-prompt via subprocess (prompt via stdin)
    # Passing the prompt through stdin avoids /proc/<pid>/cmdline exposure and
    # the Linux MAX_ARG_STRLEN limit on argument length.
    try:
        cmd = with_trace_context(
            [
                "agent-sec-cli",
                "scan-prompt",
                "--mode",
                _DEFAULT_SCAN_MODE,
                "--format",
                "json",
                "--source",
                _DEFAULT_SOURCE,
            ],
            input_data,
        )
        proc = subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            input=prompt_text,
            text=True,
            timeout=_TIMEOUT,
        )
    except (OSError, subprocess.SubprocessError, TimeoutError):
        print(_noop())
        return

    if proc.returncode != 0:
        print(_noop())
        return

    # 5. Parse ScanResult JSON from stdout
    try:
        scan_result = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        print(_noop())
        return

    if not isinstance(scan_result, dict):
        print(_noop())
        return

    # 6. Mode-based output
    verdict = scan_result.get("verdict", "pass")

    if verdict in ("pass", "error"):
        print(_noop())
        return

    # verdict is "warn" or "deny"
    if _MODE == "observe":
        print(_noop())
        return

    if _MODE == "deny":
        _block(scan_result)
        return

    # Unknown mode already handled above; keep this path fail-open.
    print(_noop())


if __name__ == "__main__":
    main()
