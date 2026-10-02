#!/usr/bin/env python3
"""Codex PreToolUse hook for code-scanner.

Reads a Codex PreToolUse JSON from stdin, extracts the shell command,
invokes ``agent-sec-cli scan-code`` via subprocess, and writes a Codex
HookOutput JSON to stdout.

Modes (controlled by CODE_SCANNER_MODE env var, default: observe):
  - observe: silent pass-through, only audit trail via agent-sec-cli events.
            Even if dangerous commands are detected, they will NOT be blocked.
  - block: block execution with reason when risk is detected.
           (agent-sec-cli's "warn" verdict is escalated to block in this mode)

Self-protect: currently disabled — no codex-specific self-protect rule exists
in agent-sec-cli yet. When shell-self-protect-codex is added, re-enable the
check below.

Usage::

    python3 code_scanner_hook.py          # reads stdin, writes stdout

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` binary on $PATH.
"""

import json
import os
import subprocess
import sys

from hook_config import env_flag_enabled, normalize_hook_policy
from trace_context import with_trace_context

# -- config ----------------------------------------------------------------

HOOK_ENABLED = env_flag_enabled("CODE_SCANNER_HOOK_ENABLED", True)
_DEFAULT_LANGUAGE = "bash"


def _diagnostic(message: str) -> None:
    print(f"[code-scanner] {' '.join(message.split())}", file=sys.stderr)


def _read_mode() -> str:
    raw = os.environ.get("CODE_SCANNER_MODE")
    mode = normalize_hook_policy(raw, "")
    if raw is not None and mode not in {"observe", "block"}:
        _diagnostic(
            f"invalid or unsupported CODE_SCANNER_MODE={raw[:32]!r}; using observe"
        )
        return "observe"
    return mode or "observe"


MODE = _read_mode()
try:
    TIMEOUT = int(os.environ.get("CODE_SCANNER_TIMEOUT", "10"))
except (ValueError, TypeError):
    TIMEOUT = 10


# -- output helpers --------------------------------------------------------


def _block(findings: list[dict]) -> None:
    """Output block decision to prevent execution (mode=block)."""
    descs = [
        f"- {f.get('rule_id', 'unknown')}: {f.get('desc_zh', f.get('desc_en', ''))}"
        for f in findings
    ]
    msg = (
        f"[code-scanner] ⛔ 安全拦截：检测到 {len(findings)} 个风险项:\n"
        + "\n".join(descs)
        + "\n\n该命令已被安全策略阻止。"
    )
    print(json.dumps({"decision": "block", "reason": msg}, ensure_ascii=False))


def _block_self_protect(command: str) -> None:
    """Force block for self-protect rule hits, regardless of mode."""
    msg = (
        "[code-scanner] 🛡️ 自我保护：该命令将禁用 agent-sec 安全插件。\n"
        "如果您确实需要禁用，请手动执行以下命令：\n\n"
        f"  {command}\n\n"
        "出于安全原因，AI agent 无法执行此操作。"
    )
    print(json.dumps({"decision": "block", "reason": msg}, ensure_ascii=False))


# -- main ------------------------------------------------------------------


def main() -> None:
    if not HOOK_ENABLED:
        return

    # 1. Read stdin JSON (fail-open: empty stdout = allow in Codex)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        return

    # 2. Extract command from tool_input
    #    Codex normalizes all shell tools to tool_name="Bash",
    #    tool_input={"command": "..."}
    tool_input = input_data.get("tool_input", {})
    command = tool_input.get("command", "")

    if not command or not isinstance(command, str) or not command.strip():
        return  # nothing to scan, allow

    # 3. Call agent-sec-cli via subprocess
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

    # 5. Self-protect check (highest priority, ignores MODE)
    # NOTE: 当前 agent-sec-cli 中仅有 shell-self-protect-hermes 和
    #       shell-self-protect-openclaw 两条规则，尚无针对 Codex 的
    #       shell-self-protect-codex 规则。startswith("shell-self-protect")
    #       可能误匹配其他 agent 的规则，因此暂时禁用此检查。
    #       待 CLI 新增 codex 专属 self-protect 规则后，可取消注释并
    #       改为精确匹配 "shell-self-protect-codex"。
    # findings_all = scan_result.get("findings", [])
    # self_protect = next(
    #     (f for f in findings_all if f.get("rule_id", "").startswith("shell-self-protect")),
    #     None,
    # )
    # if self_protect is not None:
    #     _block_self_protect(command)
    #     return

    # 6. Mode-based output
    findings = scan_result.get("findings", [])
    verdict = scan_result.get("verdict", "pass")

    if verdict in ("pass", "error"):
        return  # allow (fail-open for error)

    # verdict is "warn" or "deny":
    # - agent-sec-cli 返回 warn 表示有风险但不严重
    # - agent-sec-cli 返回 deny 表示高危
    # 在插件层统一处理：因为 Codex PreToolUse 不支持 ask/warn 透出，
    # 只能 block 或放行，所以 warn 升级为 block（与 deny 同等对待）。
    if MODE == "observe":
        return  # observe 模式：不拦截，仅通过 agent-sec-cli events 审计
    elif MODE == "block":
        _block(findings)  # warn 和 deny 均拦截
    # else: unknown mode, fail-open


if __name__ == "__main__":
    main()
