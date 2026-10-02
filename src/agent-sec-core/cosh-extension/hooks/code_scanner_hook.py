#!/usr/bin/env python3
"""Cosh hook script for code-scanner.

Reads a cosh PreToolUse JSON from stdin, extracts the shell command,
invokes ``agent-sec-cli scan-code`` via subprocess, and writes a cosh
HookOutput JSON to stdout.

Usage::

    python3 code_scanner_hook.py          # reads stdin, writes stdout

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` on $PATH.
"""

import json
import os
import subprocess
import sys

# -- extract config (mirrors cosh/extractors.py TOOL_EXTRACTORS) ----------
from hook_config import env_flag_enabled, normalize_hook_policy
from trace_context import with_trace_context

# cosh tool_name -> field in tool_input that carries the command
_TOOL_FIELD = {
    "run_shell_command": "command",
    "shell": "command",
}
_HOOK_ENABLED = env_flag_enabled("CODE_SCANNER_HOOK_ENABLED", True)
_DEFAULT_LANGUAGE = "bash"


def _diagnostic(message: str) -> None:
    """Write debug-only hook details to stderr."""
    print(f"[code-scanner] {message}", file=sys.stderr)


def _read_mode() -> str:
    raw = os.environ.get("CODE_SCANNER_MODE")
    mode = normalize_hook_policy(raw, "")
    if raw is not None and mode != "ask":
        _diagnostic(f"invalid or unsupported CODE_SCANNER_MODE={raw[:32]!r}; using ask")
    return "ask"


_MODE = _read_mode()


# -- helpers ---------------------------------------------------------------


def _allow() -> str:
    """Return a permissive cosh HookOutput JSON string."""
    return json.dumps({"decision": "allow"})


def _format_cosh(scan_result: dict) -> str:
    """Convert a ScanResult dict into a cosh HookOutput JSON string."""
    verdict = scan_result.get("verdict", "pass")
    findings = scan_result.get("findings", [])

    if verdict == "pass":
        return json.dumps({"decision": "allow"})

    descs = [f"- {f['desc_zh']}" for f in findings]
    msg = f"[code-scanner] Detected {len(findings)} issue(s):\n" + "\n".join(descs)

    if verdict in {"warn", "deny"} and _MODE == "ask":
        return json.dumps({"decision": "ask", "systemMessage": msg}, ensure_ascii=False)
    # error or unknown -> fail-open
    return json.dumps({"decision": "allow"})


# -- main ------------------------------------------------------------------


def main() -> None:
    if not _HOOK_ENABLED:
        print(_allow())
        return

    # 1. Read stdin JSON
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        print(_allow())
        return

    # 2. Extract command from tool_input
    tool_name = input_data.get("tool_name", "")
    tool_input = input_data.get("tool_input", {})

    field = _TOOL_FIELD.get(tool_name)
    if field is None:
        print(_allow())
        return

    command = tool_input.get(field)
    if not command or not isinstance(command, str) or not command.strip():
        print(_allow())
        return

    # 3. Call CLI via subprocess
    try:
        cmd = with_trace_context(
            [
                "agent-sec-cli",
                "scan-code",
                "--code",
                command,
                "--language",
                _DEFAULT_LANGUAGE,
            ],
            input_data,
        )
        proc = subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            text=True,
            timeout=10,
        )
    except Exception:
        print(_allow())
        return

    if proc.returncode != 0:
        print(_allow())
        return

    # 4. Parse ScanResult JSON from stdout
    try:
        scan_result = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        print(_allow())
        return

    # 5. Format and print cosh output
    print(_format_cosh(scan_result))


if __name__ == "__main__":
    main()
