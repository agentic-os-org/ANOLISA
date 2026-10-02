#!/usr/bin/env python3
"""Cosh hook script for prompt-scanner.

Reads a cosh UserPromptSubmit JSON from stdin, extracts the user prompt,
invokes ``agent-sec-cli scan-prompt`` via subprocess, and writes a cosh
HookOutput JSON to stdout.

Usage::

    python3 prompt_scanner_hook.py          # reads stdin, writes stdout

Hook point: **UserPromptSubmit** — fires when the user submits a prompt.
Input schema::

    {
        "session_id": "...",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "<user prompt text>"
    }

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` on $PATH.

Scan mode (controlled by ``PROMPT_SCANNER_SCAN_MODE`` env var, default:
``standard``):

- ``fast``: lightweight heuristics, lower latency.
- ``standard``: balanced detection (default).
- ``strict``: not implemented yet; currently behaves the same as standard.
"""

import json
import os
import subprocess
import sys

from hook_config import env_flag_enabled
from trace_context import with_trace_context

# -- config ----------------------------------------------------------------

_HOOK_ENABLED = env_flag_enabled("PROMPT_SCANNER_HOOK_ENABLED", True)
_RAW_SCAN_MODE = os.environ.get("PROMPT_SCANNER_SCAN_MODE", "standard").strip().lower()
_DEFAULT_MODE = _RAW_SCAN_MODE
if _DEFAULT_MODE not in {"fast", "standard", "strict"}:
    _DEFAULT_MODE = "standard"
_DEFAULT_SOURCE = "user_input"


def _warn_invalid_scan_mode() -> None:
    """Report a PROMPT_SCANNER_SCAN_MODE value that silently fell back.

    Mirrors the pii-checker and skill-ledger hooks: only the misconfiguration
    is surfaced, so a correctly configured hook stays silent.
    """
    if "PROMPT_SCANNER_SCAN_MODE" in os.environ and _RAW_SCAN_MODE != _DEFAULT_MODE:
        print(
            f"[prompt-scanner] invalid PROMPT_SCANNER_SCAN_MODE {_RAW_SCAN_MODE!r}; "
            f"using {_DEFAULT_MODE!r}",
            file=sys.stderr,
        )


# -- helpers ---------------------------------------------------------------


def _allow() -> str:
    """Return a permissive cosh HookOutput JSON string."""
    return json.dumps({"decision": "allow"})


def _build_detail_reason(scan_result: dict) -> str:
    """Build a detailed reason string from scan result for security operations."""
    threat_type = scan_result.get("threat_type", "")
    risk_level = scan_result.get("risk_level", "unknown")
    confidence = scan_result.get("confidence")

    lines = [
        "[prompt-scanner] 检测到安全风险",
        f"  攻击类型 : {threat_type or 'unknown'}",
        f"  风险等级 : {risk_level}",
        "  拦截环节 : 用户输入扫描 (UserPromptSubmit)",
    ]
    if confidence is not None:
        try:
            lines.append(f"  模型置信度: {float(confidence) * 100:.1f}%")
        except (TypeError, ValueError):
            pass

    return "\n".join(lines)


def _format_cosh(scan_result: dict) -> str:
    """Convert a ScanResult dict into a cosh HookOutput JSON string.

    Mapping:
        verdict == "pass"  -> decision "allow"
        verdict == "warn"  -> decision "ask"  (let user decide)
        verdict == "deny"  -> decision "ask"  (let user decide)
        otherwise           -> fail-open "allow"
    """
    verdict = scan_result.get("verdict", "pass")

    if verdict == "pass":
        return json.dumps({"decision": "allow"})

    reason = _build_detail_reason(scan_result)

    if verdict == "warn":
        return json.dumps(
            {"decision": "ask", "reason": reason},
            ensure_ascii=False,
        )
    # Use "ask" to avoid blocking users outright.
    # TODO: switch to "block" once the policy is mature enough.
    if verdict == "deny":
        return json.dumps(
            {"decision": "ask", "reason": reason},
            ensure_ascii=False,
        )
    # other error or unknown verdict -> fail-open
    return json.dumps({"decision": "allow"})


# -- main ------------------------------------------------------------------


def main() -> None:
    if not _HOOK_ENABLED:
        print(_allow())
        return

    _warn_invalid_scan_mode()

    # 1. Read stdin JSON (UserPromptSubmit event)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        print(_allow())
        return

    # 2. Extract user prompt text
    prompt_text = input_data.get("prompt", "")
    if not prompt_text or not isinstance(prompt_text, str) or not prompt_text.strip():
        print(_allow())
        return

    # 3. Call CLI. Model download/loading is owned by the daemon.
    # Pipe prompt via stdin (not --text argv) to avoid /proc/<pid>/cmdline
    # exposure and ARG_MAX limits — mirrors codex/hermes/qoder/qwen.
    try:
        cmd = with_trace_context(
            [
                "agent-sec-cli",
                "scan-prompt",
                "--mode",
                _DEFAULT_MODE,
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
            text=True,
            timeout=10,
            input=prompt_text,
        )
    except subprocess.TimeoutExpired as exc:
        print(
            f"[prompt-scanner] CLI timed out after {exc.timeout}s",
            file=sys.stderr,
        )
        print(_allow())
        return
    except Exception as exc:
        print(f"[prompt-scanner] CLI invocation failed: {exc}", file=sys.stderr)
        print(_allow())
        return

    if proc.returncode != 0:
        stderr_tail = (proc.stderr or "").strip().splitlines()[-5:]
        print(
            f"[prompt-scanner] CLI exited with code {proc.returncode}:"
            f" {'; '.join(stderr_tail)}",
            file=sys.stderr,
        )
        print(_allow())
        return

    # 4. Parse ScanResult JSON from stdout
    try:
        scan_result = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError) as exc:
        print(
            f"[prompt-scanner] failed to parse CLI output: {exc}",
            file=sys.stderr,
        )
        print(_allow())
        return

    # 5. Format and print cosh output
    print(_format_cosh(scan_result))


if __name__ == "__main__":
    main()
