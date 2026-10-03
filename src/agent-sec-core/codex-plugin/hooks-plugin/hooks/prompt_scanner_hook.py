#!/usr/bin/env python3
"""Codex UserPromptSubmit hook for prompt-scanner.

Reads a Codex UserPromptSubmit JSON from stdin, extracts the user prompt,
invokes ``agent-sec-cli scan-prompt`` via subprocess, and writes a Codex
HookOutput JSON to stdout.

Modes (controlled by PROMPT_SCANNER_MODE env var, default: observe):
  - observe: silent pass-through, only audit trail via agent-sec-cli events.
            Even if prompt injection is detected, it will NOT be blocked.
  - deny: block prompt with reason when risk is detected.
          (agent-sec-cli's "warn" verdict is escalated to block in this mode)

Scan mode (controlled by PROMPT_SCANNER_SCAN_MODE env var, default: standard):
  - fast: lightweight heuristics, lower latency.
  - standard: balanced detection (default).
  - strict: not implemented yet; currently behaves the same as standard.

Usage::

    python3 prompt_scanner_hook.py          # reads stdin, writes stdout

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` binary on $PATH.
"""

import json
import os
import subprocess
import sys

from hook_config import env_flag_enabled
from trace_context import with_trace_context

# -- config ----------------------------------------------------------------

_HOOK_ENABLED = env_flag_enabled("PROMPT_SCANNER_HOOK_ENABLED", True)
MODE = os.environ.get("PROMPT_SCANNER_MODE", "observe").lower()
try:
    TIMEOUT = int(os.environ.get("PROMPT_SCANNER_TIMEOUT", "10"))
except (ValueError, TypeError):
    TIMEOUT = 10
_RAW_SCAN_MODE = os.environ.get("PROMPT_SCANNER_SCAN_MODE", "standard").strip().lower()
_DEFAULT_SCAN_MODE = _RAW_SCAN_MODE
if _DEFAULT_SCAN_MODE not in {"fast", "standard", "strict"}:
    _DEFAULT_SCAN_MODE = "standard"
_DEFAULT_SOURCE = "user_input"


# -- output helpers --------------------------------------------------------


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


def _block(scan_result: dict) -> None:
    """Output block decision to reject prompt (mode=deny)."""
    threat_type = scan_result.get("threat_type", "")
    risk_level = scan_result.get("risk_level", "unknown")
    confidence = scan_result.get("confidence")

    lines = [
        "[prompt-scanner] ⛔ 安全拦截：检测到提示词注入攻击",
        f"  攻击类型 : {threat_type or 'unknown'}",
        f"  风险等级 : {risk_level}",
        "  拦截环节 : 用户输入扫描 (UserPromptSubmit)",
    ]
    if confidence is not None:
        try:
            lines.append(f"  模型置信度: {float(confidence) * 100:.1f}%")
        except (TypeError, ValueError):
            pass
    lines.append("该提示词已被安全策略阻止，请修改后重试。")
    msg = "\n".join(lines)
    print(json.dumps({"decision": "block", "reason": msg}, ensure_ascii=False))


# -- main ------------------------------------------------------------------


def main() -> None:
    if not _HOOK_ENABLED:
        return

    _warn_invalid_scan_mode()

    # 1. Read stdin JSON (fail-open: empty stdout = allow in Codex)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        return

    # 2. Extract user prompt text
    prompt_text = input_data.get("prompt", "")
    if not prompt_text or not isinstance(prompt_text, str) or not prompt_text.strip():
        return  # nothing to scan, allow

    # 3. Call agent-sec-cli scan-prompt via subprocess
    # Pass prompt via stdin (not --text) to avoid:
    # - /proc/<pid>/cmdline exposure to other system users
    # - Linux MAX_ARG_STRLEN (~128KB) limit on argument length
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
            timeout=TIMEOUT,
        )
    except Exception:
        return  # fail-open on subprocess error

    if proc.returncode != 0:
        return  # fail-open on CLI error

    # 4. Parse ScanResult JSON from stdout
    try:
        scan_result = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        return  # fail-open on parse error

    # 5. Mode-based output
    verdict = scan_result.get("verdict", "pass")

    if verdict in ("pass", "error"):
        return  # allow (fail-open for error)

    # verdict is "warn" or "deny":
    # - agent-sec-cli 返回 warn 表示有风险但不严重
    # - agent-sec-cli 返回 deny 表示高危
    # 在插件层统一处理：因为 Codex UserPromptSubmit 不支持 ask/warn 透出，
    # 只能 block 或放行，所以 warn 升级为 block（与 deny 同等对待）。
    if MODE == "observe":
        return  # observe 模式：不拦截，仅通过 agent-sec-cli events 审计
    elif MODE == "deny":
        _block(scan_result)  # warn 和 deny 均拦截，reason 展示给用户
    # else: unknown mode, fail-open


if __name__ == "__main__":
    main()
