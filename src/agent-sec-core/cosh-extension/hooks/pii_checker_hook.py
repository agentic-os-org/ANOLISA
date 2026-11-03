#!/usr/bin/env python3
"""Cosh hook script for PIIChecker.

Reads a supported cosh hook event JSON from stdin, extracts the relevant text,
invokes ``agent-sec-cli scan-pii`` via subprocess, and writes a cosh HookOutput
JSON to stdout.

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
from pii_text import value_to_text
from trace_context import with_trace_context

_HOOK_ENABLED = env_flag_enabled("PII_CHECKER_HOOK_ENABLED", True)


def _read_policy() -> str:
    """Read the configured PII Checker mode."""
    raw = os.environ.get("PII_CHECKER_MODE")
    policy = env_hook_policy("PII_CHECKER_MODE", "observe")
    if "PII_CHECKER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        print("[pii-checker] invalid PII_CHECKER_MODE; using observe", file=sys.stderr)
    return policy


_USER_INPUT_SOURCE = "user_input"
_TOOL_INPUT_SOURCE = "tool_input"
_TOOL_OUTPUT_SOURCE = "tool_output"
_MODEL_OUTPUT_SOURCE = "model_output"


def _allow() -> str:
    """Return a permissive cosh HookOutput JSON string."""
    return json.dumps({"decision": "allow"})


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


def _format_pii_warning(
    verdict: str,
    findings: list[Any],
    action_message: str = "本次仅提醒，未触发确认或阻断。",
    summary: Any = None,
) -> str:
    """Build a concise warning without exposing internal labels or evidence."""
    # Cosh strips this exact hook-name prefix from permissive notifications.
    return f"[pii-checker] {_risk_summary(verdict, findings, summary=summary)}；{action_message}"


def _scan_text(
    input_data: dict[str, Any], text: str, source: str
) -> dict[str, Any] | None:
    """Run scan-pii with a source label and parse JSON output."""
    try:
        cmd = with_trace_context(
            [
                "agent-sec-cli",
                "scan-pii",
                "--stdin",
                "--format",
                "json",
                "--redact-output",
                "--source",
                source,
            ],
            input_data,
        )
        proc = subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            input=text,
            text=True,
            timeout=10,
        )
    except Exception:
        return None

    if proc.returncode != 0:
        return None

    try:
        scan_result = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        return None
    return scan_result if isinstance(scan_result, dict) else None


def _extract_response_text(llm_response: Any) -> str:
    """Extract text from common Cosh AfterModel response shapes."""
    if isinstance(llm_response, str):
        return llm_response
    if not isinstance(llm_response, dict):
        return ""

    text = llm_response.get("text")
    if isinstance(text, str):
        return text

    candidates = llm_response.get("candidates")
    if not isinstance(candidates, list):
        return ""

    parts: list[str] = []
    for candidate in candidates:
        if not isinstance(candidate, dict):
            continue
        content = candidate.get("content")
        if not isinstance(content, dict):
            continue
        candidate_parts = content.get("parts")
        if isinstance(candidate_parts, str):
            parts.append(candidate_parts)
        elif isinstance(candidate_parts, list):
            for part in candidate_parts:
                if isinstance(part, str):
                    parts.append(part)
                elif isinstance(part, dict) and isinstance(part.get("text"), str):
                    parts.append(part["text"])
    return "".join(parts)


def _hook_event_name(input_data: dict[str, Any]) -> str:
    """Return the canonical Cosh event name, preserving legacy input behavior."""
    event_name = _safe_text(input_data.get("hook_event_name"))
    if not event_name:
        event_name = _safe_text(input_data.get("hookEventName"))
    return event_name or "UserPromptSubmit"


def _extract_scan_target(
    input_data: dict[str, Any], event_name: str | None = None
) -> tuple[str, str]:
    """Return text and source for supported Cosh hook events."""
    event_name = event_name or _hook_event_name(input_data)

    if event_name == "UserPromptSubmit":
        return _safe_text(input_data.get("prompt")), _USER_INPUT_SOURCE

    if event_name == "PreToolUse":
        if "tool_input" not in input_data:
            return "", _TOOL_INPUT_SOURCE
        return value_to_text(input_data.get("tool_input")), _TOOL_INPUT_SOURCE

    if event_name == "PostToolUse":
        if "tool_response" not in input_data:
            return "", _TOOL_OUTPUT_SOURCE
        return value_to_text(input_data.get("tool_response")), _TOOL_OUTPUT_SOURCE

    if event_name == "PostToolUseFailure":
        return _safe_text(input_data.get("error")), _TOOL_OUTPUT_SOURCE

    if event_name == "AfterModel":
        return (
            _extract_response_text(input_data.get("llm_response")),
            _MODEL_OUTPUT_SOURCE,
        )

    return "", "unknown"


def _format_cosh(
    scan_result: dict[str, Any],
    policy: str = "warn",
    event_name: str = "UserPromptSubmit",
) -> str:
    """Convert a scan-pii result dict into a cosh HookOutput JSON string.

    Mapping:
        verdict == "pass" -> decision "allow"
        verdict == "warn" -> decision "allow" with a warning when policy is active
        verdict == "deny" -> event-specific ask/block or a warning fallback
        verdict == "error" or unknown -> fail-open "allow"
    """
    verdict = _safe_text(scan_result.get("verdict")) or "pass"
    findings = [
        finding
        for finding in _as_list(scan_result.get("findings"))
        if isinstance(finding, dict)
    ]

    if verdict == "pass" or not findings:
        return _allow()

    if policy == "observe" or verdict not in {"warn", "deny"}:
        return _allow()

    decision = "allow"
    action_message = "本次仅提醒，未触发确认或阻断。"
    if event_name in {"PostToolUse", "PostToolUseFailure"}:
        action_message = (
            "工具已经执行；本次仅提醒，未触发确认或阻断；"
            "原始工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。"
        )

    # A scanner warning never escalates into confirmation or blocking.
    if verdict == "deny" and policy == "ask":
        if event_name == "UserPromptSubmit":
            decision = "ask"
            action_message = "当前策略要求确认，请确认后继续。"
        elif event_name == "PreToolUse":
            decision = "ask"
            action_message = "当前策略要求确认，请确认后继续。"
        elif event_name in {"PostToolUse", "PostToolUseFailure"}:
            action_message = (
                "工具已经执行；当前环节不支持确认/阻断，本次仅提醒，不会阻断；"
                "原始工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。"
            )
        else:
            action_message = "当前环节不支持确认/阻断，本次仅提醒，不会阻断。"
    elif verdict == "deny" and policy == "block":
        if event_name == "UserPromptSubmit":
            decision = "block"
            action_message = "当前策略已阻断本次请求。"
        elif event_name == "PreToolUse":
            decision = "block"
            action_message = "当前策略已阻断本次工具调用。"
        elif event_name == "PostToolUse":
            decision = "block"
            action_message = (
                "工具已经执行；原始工具结果不会进入模型上下文，"
                "已发生的外部副作用不会撤销。"
            )
        elif event_name == "PostToolUseFailure":
            action_message = (
                "工具已经执行；当前环节不支持确认/阻断，本次仅提醒，不会阻断；"
                "原始工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。"
            )
        else:
            action_message = "当前环节不支持确认/阻断，本次仅提醒，不会阻断。"

    return json.dumps(
        {
            "decision": decision,
            "reason": _format_pii_warning(
                verdict, findings, action_message, summary=scan_result.get("summary")
            ),
        },
        ensure_ascii=False,
    )


def main() -> None:
    if not _HOOK_ENABLED:
        print(_allow())
        return

    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        print(_allow())
        return

    if not isinstance(input_data, dict):
        print(_allow())
        return

    event_name = _hook_event_name(input_data)
    scan_text, source = _extract_scan_target(input_data, event_name)
    if not isinstance(scan_text, str) or not scan_text.strip():
        print(_allow())
        return

    scan_result = _scan_text(input_data, scan_text, source)
    if scan_result is None:
        print(_allow())
        return
    print(_format_cosh(scan_result, _read_policy(), event_name))


if __name__ == "__main__":
    main()
