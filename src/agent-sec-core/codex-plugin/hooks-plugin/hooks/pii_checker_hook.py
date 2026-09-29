#!/usr/bin/env python3
"""Codex hook for PII (Personal Identifiable Information) detection.

Supports THREE hook points via a single script (routed by hook_event_name):
  - UserPromptSubmit: scans user prompt before it reaches the model.
  - PreToolUse: scans tool input before the tool executes.
  - PostToolUse: scans tool output before it enters model context.

Protection direction:
  - UserPromptSubmit / PostToolUse: detect PII flowing INTO the LLM provider
    (user prompt / tool output → model) before applying the configured policy.
  - PreToolUse: detect PII flowing OUT via a tool call (exfiltration), e.g.
    curl-ing a phone number to an external endpoint or writing PII to a file.
    This is the only point to enforce PII policy before the tool executes.

Policy (controlled by PII_CHECKER_MODE, default: observe):
  - observe: silent pass-through, only audit trail via agent-sec-cli events.
             Even if PII is detected, content will NOT be blocked.
  - warn: surface scanner findings through systemMessage and continue.
  - ask: fall back to warn when this Codex hook cannot request confirmation.
  - block: block scanner "deny" at a controllable boundary; post-tool blocking
           cannot undo side effects that already occurred.

The compatibility values debug and deny map to observe and block, respectively.

Protocol note: Codex supports non-blocking systemMessage warnings but does not
support "redact and pass" for these hook points. A warning therefore forwards
the original payload unchanged, while a deny verdict blocks the payload.

Usage::

    python3 pii_checker_hook.py          # reads stdin, writes stdout

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package. All it needs is the standard library and the
``agent-sec-cli`` binary on $PATH.
"""

import json
import os
import subprocess
import sys
from typing import Any

from hook_config import env_flag_enabled, env_hook_policy, normalize_hook_policy
from trace_context import with_trace_context

# -- config ----------------------------------------------------------------

_HOOK_ENABLED = env_flag_enabled("PII_CHECKER_HOOK_ENABLED", True)
MODE = os.environ.get("PII_CHECKER_MODE")


def _read_policy() -> str:
    """Read the configured PII Checker mode."""
    raw = os.environ.get("PII_CHECKER_MODE")
    policy = env_hook_policy("PII_CHECKER_MODE", "observe")
    if "PII_CHECKER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        print("[pii-checker] invalid PII_CHECKER_MODE; using observe", file=sys.stderr)
    return policy


_POLICY = _read_policy()


def _effective_policy() -> str:
    """Return policy while preserving module-level test overrides."""
    return normalize_hook_policy(MODE, _POLICY)


try:
    TIMEOUT = int(os.environ.get("PII_CHECKER_TIMEOUT", "5"))
except (ValueError, TypeError):
    TIMEOUT = 5

# -- helpers ---------------------------------------------------------------


def _as_list(value: Any) -> list[Any]:
    return value if isinstance(value, list) else []


def _safe_text(value: Any) -> str:
    return value if isinstance(value, str) else ""


def _finding_risk(finding: Any, verdict: str) -> str:
    """Return the user-facing risk for a structured scanner finding."""
    if isinstance(finding, dict):
        severity = _safe_text(finding.get("severity"))
        if severity == "deny":
            return "high"
        if severity == "warn":
            return "general"

    return "high" if verdict == "deny" else "general"


def _risk_summary(verdict: str, findings: list[Any], summary: Any = None) -> str:
    """Summarize finding counts without exposing scanner-internal fields."""
    if isinstance(summary, dict) and summary.get("findings_truncated") is True:
        counts = summary.get("by_severity")
        if isinstance(counts, dict):
            high, general = counts.get("deny", 0), counts.get("warn", 0)
            total = summary.get("total")
            if (
                all(
                    type(value) is int and value >= 0
                    for value in (high, general, total)
                )
                and total == high + general
            ):
                return (
                    f"检测到 {total} 项敏感信息"
                    f"（高风险 {high}、一般风险 {general}；明细已省略）"
                )
    typed_findings = [finding for finding in findings if isinstance(finding, dict)]
    high_count = sum(
        1 for finding in typed_findings if _finding_risk(finding, verdict) == "high"
    )
    general_count = len(typed_findings) - high_count

    if high_count and general_count:
        return (
            f"检测到 {len(typed_findings)} 项敏感信息"
            f"（高风险 {high_count}、一般风险 {general_count}）"
        )
    if high_count:
        return f"检测到 {high_count} 项高风险敏感信息"
    return f"检测到 {general_count} 项一般风险敏感信息"


# -- output helpers --------------------------------------------------------


def _format_notice(
    verdict: str, findings: list[Any], action_message: str, summary: Any = None
) -> str:
    """Build a concise notice without exposing internal labels or evidence."""
    return f"[pii-checker] {_risk_summary(verdict, findings, summary=summary)}；{action_message}"


def _format_block_reason(
    findings: list[Any], hook_event: str, verdict: str, summary: Any = None
) -> str:
    """Build an event-specific block reason for the user."""
    if hook_event == "UserPromptSubmit":
        action_message = "当前策略已阻断本次请求。"
    elif hook_event == "PreToolUse":
        action_message = "当前策略已阻断本次工具调用。"
    else:
        action_message = (
            "工具已经执行；原始工具结果不会进入模型上下文，"
            "已发生的外部副作用不会撤销。"
        )

    return _format_notice(verdict, findings, action_message, summary=summary)


def _format_warning_message(
    findings: list[Any], hook_event: str, verdict: str, policy: str, summary: Any = None
) -> str:
    """Build a concise warning that states the actual non-blocking behavior."""
    if hook_event == "PostToolUse":
        if verdict == "deny" and policy == "ask":
            action_message = (
                "工具已经执行；当前环节不支持确认/阻断，本次仅提醒，不会阻断；"
                "原始工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。"
            )
        else:
            action_message = (
                "工具已经执行；本次仅提醒，未触发确认或阻断；"
                "原始工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。"
            )
    elif verdict == "deny" and policy == "ask":
        action_message = "当前环节不支持确认/阻断，本次仅提醒，不会阻断。"
    else:
        action_message = "本次仅提醒，未触发确认或阻断。"
    return _format_notice(verdict, findings, action_message, summary=summary)


def _block(
    findings: list[Any], hook_event: str, verdict: str, summary: Any = None
) -> None:
    """Output block decision JSON to stdout."""
    reason = _format_block_reason(findings, hook_event, verdict, summary=summary)
    print(json.dumps({"decision": "block", "reason": reason}, ensure_ascii=False))


def _warn(
    findings: list[Any], hook_event: str, verdict: str, policy: str, summary: Any = None
) -> None:
    """Output a user-visible warning without changing execution control."""
    message = _format_warning_message(
        findings, hook_event, verdict, policy, summary=summary
    )
    print(json.dumps({"systemMessage": message}, ensure_ascii=False))


# -- text extraction -------------------------------------------------------


def _extract_scan_text(input_data: dict, hook_event: str) -> str | None:
    """Extract the text to scan based on hook event type.

    Returns None if there's nothing meaningful to scan.
    """
    if hook_event == "UserPromptSubmit":
        text = input_data.get("prompt", "")
        if isinstance(text, str) and text.strip():
            return text
        return None

    if hook_event == "PreToolUse":
        tool_input = input_data.get("tool_input")
        if tool_input is None:
            return None
        # tool_input is a serde_json::Value — could be string, object, array
        if isinstance(tool_input, str):
            return tool_input if tool_input.strip() else None
        # For non-string types, serialize to text for scanning
        try:
            text = json.dumps(tool_input, ensure_ascii=False)
        except (TypeError, ValueError):
            return None
        # Empty containers serialize to non-empty strings ("{}", "[]",
        # "null") but carry no PII — skip to avoid a wasted scan-pii call.
        if not text.strip() or text in ("{}", "[]", "null"):
            return None
        return text

    if hook_event == "PostToolUse":
        tool_response = input_data.get("tool_response")
        if tool_response is None:
            return None
        # tool_response is a serde_json::Value — could be string, object, array
        if isinstance(tool_response, str):
            return tool_response if tool_response.strip() else None
        # For non-string types, serialize to text for scanning
        try:
            text = json.dumps(tool_response, ensure_ascii=False)
        except (TypeError, ValueError):
            return None
        # Empty containers serialize to non-empty strings ("{}", "[]",
        # "null") but carry no PII — skip to avoid a wasted scan-pii call.
        if not text.strip() or text in ("{}", "[]", "null"):
            return None
        return text

    return None


def _source_for_event(hook_event: str) -> str:
    """Return the --source argument value for agent-sec-cli."""
    if hook_event == "PreToolUse":
        return "tool_input"
    if hook_event == "PostToolUse":
        return "tool_output"
    return "user_input"


# -- main ------------------------------------------------------------------


def main() -> None:
    if not _HOOK_ENABLED:
        return

    # 1. Read stdin JSON (fail-open: empty stdout = allow in Codex)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        return

    # 2. Determine which hook event we're handling
    hook_event = input_data.get("hook_event_name", "")
    if hook_event not in ("UserPromptSubmit", "PreToolUse", "PostToolUse"):
        return  # unknown event, fail-open

    # 3. Extract text to scan
    scan_text = _extract_scan_text(input_data, hook_event)
    if not scan_text:
        return  # nothing to scan, allow

    # 4. Call agent-sec-cli scan-pii via subprocess
    source = _source_for_event(hook_event)
    try:
        cmd = with_trace_context(
            [
                "agent-sec-cli",
                "scan-pii",
                "--stdin",
                "--format",
                "json",
                "--source",
                source,
            ],
            input_data,
        )
        proc = subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            input=scan_text,
            text=True,
            timeout=TIMEOUT,
        )
    except Exception:
        return  # fail-open on subprocess error

    if proc.returncode != 0:
        return  # fail-open on CLI error

    # 5. Parse scan result JSON from stdout
    try:
        scan_result = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        return  # fail-open on parse error

    # 6. Mode-based output
    verdict = _safe_text(scan_result.get("verdict")) or "pass"
    findings = _as_list(scan_result.get("findings"))

    if verdict == "pass" or not findings:
        return  # no PII detected, allow
    if verdict not in {"warn", "deny"}:
        return

    policy = _effective_policy()
    if policy == "observe":
        return  # observe mode: don't block, audit only via CLI events
    if policy == "block" and verdict == "deny":
        _block(findings, hook_event, verdict, summary=scan_result.get("summary"))
        return
    # Codex cannot request approval at all PII hook points; ask falls back to warn.
    _warn(findings, hook_event, verdict, policy, summary=scan_result.get("summary"))


if __name__ == "__main__":
    main()
