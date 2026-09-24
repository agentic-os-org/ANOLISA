#!/usr/bin/env python3
"""Qoder hook for PII and credential detection."""

import json
import os
import subprocess
from typing import Any

from qoder_hook_common import (
    deny_output,
    dumps_hook_output,
    env_flag_enabled,
    jsonish_value,
    load_hook_input,
    normalize_hook_policy,
    post_tool_output_replacement,
    pre_tool_decision_output,
    value_to_text,
    with_trace_context,
)

_HOOK_ENABLED = env_flag_enabled("PII_CHECKER_HOOK_ENABLED", True)
_POLICY_ENV_NAME = "PII_CHECKER_MODE"
_POLICY_RAW = os.environ.get(_POLICY_ENV_NAME)
_POLICY = normalize_hook_policy(_POLICY_RAW, "observe")
try:
    _TIMEOUT = int(os.environ.get("PII_CHECKER_TIMEOUT", "5"))
except (TypeError, ValueError):
    _TIMEOUT = 5

_INCLUDE_LOW_CONFIDENCE = os.environ.get(
    "PII_CHECKER_INCLUDE_LOW_CONFIDENCE", ""
).strip().lower() in {"1", "true", "yes", "on"}
_USER_INPUT_SOURCE = "user_input"
_TOOL_INPUT_SOURCE = "tool_input"
_TOOL_OUTPUT_SOURCE = "tool_output"


def _safe_string(value: Any) -> str:
    """Return value when it is a string, otherwise an empty string."""
    return value if isinstance(value, str) else ""


def _as_list(value: Any) -> list[Any]:
    """Return value when it is a list, otherwise an empty list."""
    return value if isinstance(value, list) else []


def _hook_event(input_data: dict[str, Any]) -> str:
    """Return the Qoder hook event name."""
    return _safe_string(input_data.get("hook_event_name"))


def _scan_target(input_data: dict[str, Any]) -> tuple[str, str] | None:
    """Return text and source label for supported Qoder hooks."""
    event_name = _hook_event(input_data)
    if event_name == "UserPromptSubmit":
        text = _safe_string(input_data.get("prompt"))
        return (text, _USER_INPUT_SOURCE) if text.strip() else None

    if event_name == "PreToolUse":
        if "tool_input" not in input_data:
            return None
        text = value_to_text(jsonish_value(input_data.get("tool_input")))
        return (text, _TOOL_INPUT_SOURCE) if text.strip() else None

    if event_name == "PostToolUse":
        if "tool_response" not in input_data:
            return None
        text = value_to_text(jsonish_value(input_data.get("tool_response")))
        return (text, _TOOL_OUTPUT_SOURCE) if text.strip() else None

    return None


def _scan_pii(
    input_data: dict[str, Any],
    text: str,
    source: str,
) -> dict[str, Any] | None:
    """Run agent-sec-cli scan-pii and parse its JSON response."""
    args = [
        "agent-sec-cli",
        "scan-pii",
        "--stdin",
        "--format",
        "json",
        "--redact-output",
        "--source",
        source,
    ]
    if _INCLUDE_LOW_CONFIDENCE:
        args.append("--include-low-confidence")

    try:
        proc = subprocess.run(
            with_trace_context(args, input_data),
            capture_output=True,
            check=False,
            input=text,
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


def _risk_summary(verdict: str, findings: list[Any], summary: Any = None) -> str:
    """Summarize finding counts without exposing scanner-internal details."""
    if isinstance(summary, dict) and summary.get("findings_truncated") is True:
        counts = summary.get("by_severity")
        if isinstance(counts, dict):
            high, general = counts.get("deny", 0), counts.get("warn", 0)
            total = summary.get("total")
            if (
                all(type(value) is int and value >= 0 for value in (high, general, total))
                and total == high + general
            ):
                return (
                    f"Detected {total} sensitive data findings "
                    f"({high} high risk, {general} general risk; details omitted)"
                )
    typed_findings = [item for item in findings if isinstance(item, dict)]
    high_count = 0
    general_count = 0
    for finding in typed_findings:
        severity = _safe_string(finding.get("severity"))
        if severity == "deny":
            high_count += 1
        elif severity == "warn":
            general_count += 1

    unknown_count = len(typed_findings) - high_count - general_count
    if verdict == "deny":
        high_count += unknown_count
    else:
        general_count += unknown_count

    total = len(typed_findings)
    noun = "finding" if total == 1 else "findings"
    if high_count and general_count:
        return (
            f"Detected {total} sensitive data {noun} "
            f"({high_count} high risk, {general_count} general risk)"
        )
    risk = "high-risk" if high_count else "general-risk"
    return f"Detected {total} {risk} sensitive data {noun}"


def _format_notice(verdict: str, findings: list[Any], action: str, summary: Any = None) -> str:
    """Build a minimal-disclosure PII notice with the actual host action."""
    return f"[pii-checker] {_risk_summary(verdict, findings, summary=summary)}. {action}"


def _warning_action(event_name: str) -> str:
    """Describe the event-specific result of a warning-only decision."""
    if event_name == "PreToolUse":
        return (
            "This is a warning only; the tool call will continue without confirmation "
            "or blocking."
        )
    if event_name == "PostToolUse":
        return (
            "The tool has already run. This is a warning only; its raw output will enter "
            "model context, and external side effects were not undone."
        )
    return "This is a warning only; the request will continue without confirmation or " "blocking."


def _unsupported_confirmation_action(event_name: str) -> str:
    """Describe a warning when the current hook cannot request confirmation."""
    if event_name == "PostToolUse":
        return (
            "The tool has already run. This stage cannot confirm or block; this is a "
            "warning only, its raw output will enter model context, and external side "
            "effects were not undone."
        )
    return (
        "This stage cannot confirm or block; this is a warning only and the request will "
        "continue."
    )


def _warn_output(notice: str) -> str:
    """Return an allow decision with a user-visible system message."""
    return dumps_hook_output({"decision": "allow", "systemMessage": notice})


def _invalid_policy_output() -> str:
    """Return a visible fail-open warning for an invalid checker policy."""
    notice = (
        "[pii-checker] PII Checker configuration is invalid; processing will continue "
        "without confirmation or blocking."
    )
    return _warn_output(notice)


def _format_decision(
    input_data: dict[str, Any], verdict: str, findings: list[Any], summary: Any = None
) -> str | None:
    """Map a scan-pii verdict to Qoder hook output."""
    if verdict == "pass" or not findings:
        return None
    if verdict not in {"warn", "deny"}:
        return None
    if _POLICY == "observe":
        return None

    event_name = _hook_event(input_data)
    if verdict == "warn" or _POLICY == "warn":
        notice = _format_notice(verdict, findings, _warning_action(event_name), summary=summary)
        return _warn_output(notice)

    if _POLICY == "ask" and event_name == "PreToolUse":
        notice = _format_notice(
            verdict,
            findings,
            "Confirmation is required before this tool call can continue.",
            summary=summary,
        )
        return pre_tool_decision_output("ask", notice)

    if _POLICY == "ask":
        notice = _format_notice(
            verdict,
            findings,
            _unsupported_confirmation_action(event_name),
            summary=summary,
        )
        return _warn_output(notice)

    if event_name == "UserPromptSubmit":
        notice = _format_notice(
            verdict,
            findings,
            "The current protection settings blocked this request.",
            summary=summary,
        )
        return deny_output(notice)

    if event_name == "PreToolUse":
        notice = _format_notice(
            verdict,
            findings,
            "The current protection settings blocked this tool call.",
            summary=summary,
        )
        return pre_tool_decision_output("deny", notice)

    if event_name == "PostToolUse":
        notice = _format_notice(
            verdict,
            findings,
            "The tool has already run. Its raw output will not enter model context; "
            "external side effects were not undone.",
            summary=summary,
        )
        return post_tool_output_replacement(notice)

    return None


def main() -> None:
    """Run the Qoder PII hook."""
    if not _HOOK_ENABLED:
        return

    input_data = load_hook_input()
    if input_data is None:
        return

    target = _scan_target(input_data)
    if target is None:
        return
    text, source = target

    scan_result = _scan_pii(input_data, text, source)
    if _POLICY_RAW is not None and normalize_hook_policy(_POLICY_RAW, "") == "":
        print(_invalid_policy_output())
        return
    if scan_result is None:
        return

    verdict = _safe_string(scan_result.get("verdict")) or "pass"
    findings = _as_list(scan_result.get("findings"))
    output = _format_decision(input_data, verdict, findings, summary=scan_result.get("summary"))
    if output:
        print(output)


if __name__ == "__main__":
    main()
