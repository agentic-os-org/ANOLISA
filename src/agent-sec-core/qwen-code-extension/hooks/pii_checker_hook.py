#!/usr/bin/env python3
"""Scan Qwen Code hook content for PII and credentials.

Qwen Code compatibility contract
--------------------------------
This hook targets the Qwen Code 0.19.9 protocol:

* ``PostToolUse`` stops downstream handling through ``continue=false``;
  ``decision=block`` alone is not consumed by that version.
* ``PostToolUseFailure`` and ``StopFailure`` do not consume blocking output, so
  they remain scan-and-audit only.
* ``stop_hook_active`` identifies a repeated ``Stop`` pass, which must not be
  blocked again or it can create a rewrite loop.
* Qwen Code has no pre-render output-transform hook, so final-output blocking is
  best effort.

The 0.19.9 HookInput does not expose a runtime version field. Compatibility is
therefore pinned by protocol tests instead of inferred inside the hook.
"""

import json
import math
import os
import subprocess
import sys
from typing import Any

from hook_config import env_flag_enabled, env_hook_policy, normalize_hook_policy
from pii_text import value_to_text
from trace_context import with_trace_context

_DEFAULT_TIMEOUT_SECONDS = 5.0
_MAX_TIMEOUT_SECONDS = 8.0
_MAX_PAYLOAD_SIZE = 1024 * 1024
_TRUE_VALUES = {"1", "true", "yes", "on"}
_FALSE_VALUES = {"0", "false", "no", "off"}
_VALID_VERDICTS = {"pass", "warn", "deny", "error"}

_USER_INPUT_SOURCE = "user_input"
_TOOL_INPUT_SOURCE = "tool_input"
_TOOL_OUTPUT_SOURCE = "tool_output"
_MODEL_OUTPUT_SOURCE = "model_output"


def _noop() -> dict[str, Any]:
    """Return a Qwen Code HookOutput that does not alter execution."""
    return {}


def _environment_bool(name: str, default: bool) -> bool:
    value = os.environ.get(name)
    if value is None:
        return default
    normalized = value.strip().lower()
    if normalized in _TRUE_VALUES:
        return True
    if normalized in _FALSE_VALUES:
        return False
    return default


def _policy() -> str:
    raw = os.environ.get("PII_CHECKER_MODE")
    policy = env_hook_policy("PII_CHECKER_MODE", "observe")
    if "PII_CHECKER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        print(
            "[pii-checker] PII Checker configuration is invalid; processing will "
            "continue without confirmation or blocking.",
            file=sys.stderr,
        )
    return policy


def _mode() -> str:
    """Return the configured PII Checker mode."""
    return _policy()


def _timeout_seconds() -> float:
    try:
        value = float(os.environ.get("PII_CHECKER_TIMEOUT", _DEFAULT_TIMEOUT_SECONDS))
    except (TypeError, ValueError):
        return _DEFAULT_TIMEOUT_SECONDS
    if not math.isfinite(value) or value <= 0:
        return _DEFAULT_TIMEOUT_SECONDS
    return min(value, _MAX_TIMEOUT_SECONDS)


def _read_hook_input() -> dict[str, Any] | None:
    """Read one bounded Qwen Code HookInput object from stdin."""
    try:
        stream = getattr(sys.stdin, "buffer", sys.stdin)
        payload = stream.read(_MAX_PAYLOAD_SIZE + 1)
        if len(payload) > _MAX_PAYLOAD_SIZE:
            return None
        input_data = json.loads(payload)
    except (json.JSONDecodeError, EOFError, OSError, TypeError, ValueError):
        return None
    return input_data if isinstance(input_data, dict) else None


def _string(value: Any) -> str:
    return value if isinstance(value, str) else ""


def _scan_target(input_data: dict[str, Any]) -> tuple[str, str, str] | None:
    """Return event name, scanner source, and exact text for supported hooks."""
    event_name = _string(input_data.get("hook_event_name"))
    if event_name == "UserPromptSubmit":
        text = _string(input_data.get("prompt"))
        return (event_name, _USER_INPUT_SOURCE, text) if text.strip() else None
    if event_name == "PreToolUse" and "tool_input" in input_data:
        text = value_to_text(input_data.get("tool_input"))
        return (event_name, _TOOL_INPUT_SOURCE, text) if text.strip() else None
    if event_name == "PostToolUse" and "tool_response" in input_data:
        text = value_to_text(input_data.get("tool_response"))
        return (event_name, _TOOL_OUTPUT_SOURCE, text) if text.strip() else None
    if event_name == "PostToolUseFailure":
        text = _string(input_data.get("error"))
        return (event_name, _TOOL_OUTPUT_SOURCE, text) if text.strip() else None
    if event_name in {"Stop", "StopFailure"}:
        text = _string(input_data.get("last_assistant_message"))
        return (event_name, _MODEL_OUTPUT_SOURCE, text) if text.strip() else None
    return None


def _scan_pii(
    input_data: dict[str, Any], text: str, source: str
) -> dict[str, Any] | None:
    """Run scan-pii with raw content on stdin and parse its JSON response."""
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
    if _environment_bool("PII_CHECKER_INCLUDE_LOW_CONFIDENCE", False):
        args.append("--include-low-confidence")

    try:
        result = subprocess.run(
            with_trace_context(args, input_data),
            input=text,
            capture_output=True,
            text=True,
            timeout=_timeout_seconds(),
            check=False,
        )
    except (OSError, subprocess.SubprocessError, TimeoutError):
        return None
    if result.returncode != 0:
        return None

    try:
        scan_result = json.loads(result.stdout)
    except (json.JSONDecodeError, TypeError, ValueError):
        return None
    return scan_result if isinstance(scan_result, dict) else None


def _validated_result(
    scan_result: dict[str, Any],
) -> tuple[str, list[dict[str, str]]] | None:
    """Return a supported verdict with only fields needed for risk counts."""
    verdict = scan_result.get("verdict")
    if not isinstance(verdict, str) or verdict not in _VALID_VERDICTS:
        return None
    if verdict == "pass":
        return verdict, []
    if verdict == "error":
        return None

    findings = scan_result.get("findings")
    if not isinstance(findings, list):
        return None
    sanitized_findings: list[dict[str, str]] = []
    has_redacted_evidence = False
    for finding in findings:
        if not isinstance(finding, dict):
            continue
        redacted = finding.get("evidence_redacted")
        if isinstance(redacted, str) and redacted.strip():
            has_redacted_evidence = True
        severity = finding.get("severity")
        sanitized_findings.append(
            {"severity": severity} if isinstance(severity, str) else {}
        )
    if not sanitized_findings or not has_redacted_evidence:
        return None
    return verdict, sanitized_findings


def _risk_summary(
    verdict: str, findings: list[dict[str, str]], summary: Any = None
) -> str:
    """Summarize finding counts without exposing scanner-internal details."""
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
                    f"Detected {total} sensitive data findings "
                    f"({high} high risk, {general} general risk; details omitted)"
                )
    high_count = sum(finding.get("severity") == "deny" for finding in findings)
    general_count = sum(finding.get("severity") == "warn" for finding in findings)
    unknown_count = len(findings) - high_count - general_count
    if verdict == "deny":
        high_count += unknown_count
    else:
        general_count += unknown_count

    total = len(findings)
    noun = "finding" if total == 1 else "findings"
    if high_count and general_count:
        return (
            f"Detected {total} sensitive data {noun} "
            f"({high_count} high risk, {general_count} general risk)"
        )
    risk = "high-risk" if high_count else "general-risk"
    return f"Detected {total} {risk} sensitive data {noun}"


def _notice(
    verdict: str, findings: list[dict[str, str]], action: str, summary: Any = None
) -> str:
    return (
        f"[pii-checker] {_risk_summary(verdict, findings, summary=summary)}. {action}"
    )


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
    if event_name == "Stop":
        return "This is a warning only; the response will continue without blocking."
    return (
        "This is a warning only; the request will continue without confirmation or "
        "blocking."
    )


def _unsupported_confirmation_action(event_name: str) -> str:
    """Describe a warning when the current hook cannot request confirmation."""
    if event_name == "PostToolUse":
        return (
            "The tool has already run. This stage cannot confirm or block; this is a "
            "warning only, its raw output will enter model context, and external side "
            "effects were not undone."
        )
    if event_name == "Stop":
        return (
            "This stage cannot confirm or block; this is a warning only and the response "
            "will continue."
        )
    return (
        "This stage cannot confirm or block; this is a warning only and the request will "
        "continue."
    )


def _decision(
    input_data: dict[str, Any],
    event_name: str,
    verdict: str,
    findings: list[dict[str, str]],
    summary: Any = None,
) -> dict[str, Any]:
    """Map one validated scanner verdict to Qwen Code HookOutput."""
    if event_name in {"PostToolUseFailure", "StopFailure"}:
        # Qwen Code 0.19.9 does not consume control fields for these failure
        # events, so scanning remains audit-only instead of claiming a block.
        return _noop()

    policy = _policy()
    if policy == "observe":
        return _noop()
    if verdict == "warn" or policy == "warn":
        return {
            "systemMessage": _notice(
                verdict, findings, _warning_action(event_name), summary=summary
            ),
        }

    if policy == "ask" and event_name == "PreToolUse":
        reason = _notice(
            verdict,
            findings,
            "Confirmation is required before this tool call can continue.",
            summary=summary,
        )
        return {
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "permissionDecision": "ask",
                "permissionDecisionReason": reason,
            }
        }
    if policy == "ask":
        return {
            "systemMessage": _notice(
                verdict,
                findings,
                _unsupported_confirmation_action(event_name),
                summary=summary,
            )
        }

    if event_name == "UserPromptSubmit":
        reason = _notice(
            verdict,
            findings,
            "The current protection settings blocked this request.",
            summary=summary,
        )
        return {
            "decision": "block",
            "reason": reason,
            "hookSpecificOutput": {"hookEventName": event_name},
        }
    if event_name == "PreToolUse":
        reason = _notice(
            verdict,
            findings,
            "The current protection settings blocked this tool call.",
            summary=summary,
        )
        return {
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }
    if event_name == "PostToolUse":
        reason = _notice(
            verdict,
            findings,
            "The tool has already run. Its raw output will not enter model context; "
            "external side effects were not undone.",
            summary=summary,
        )
        # Qwen Code 0.19.9 only stops PostToolUse through
        # shouldStopExecution(), which checks continue=false. Keep the
        # documented decision fields for policy intent and future compatibility.
        return {
            "continue": False,
            "stopReason": reason,
            "decision": "block",
            "reason": reason,
        }
    if event_name == "Stop":
        if input_data.get("stop_hook_active") is True:
            return {
                "systemMessage": _notice(
                    verdict,
                    findings,
                    "This pass is warning-only and will not block the response again, "
                    "avoiding a retry loop.",
                    summary=summary,
                )
            }
        reason = _notice(
            verdict,
            findings,
            "The final response was blocked. Rewrite it without sensitive data, then try "
            "again.",
            summary=summary,
        )
        return {"decision": "block", "reason": reason}
    return _noop()


def main() -> None:
    """Run the Qwen Code PII checker hook with fail-open behavior."""
    try:
        if "PII_CHECKER_HOOK_ENABLED" in os.environ:
            enabled = env_flag_enabled("PII_CHECKER_HOOK_ENABLED", True)
        else:
            enabled = _environment_bool("PII_CHECKER_ENABLED", True)
        if not enabled:
            print(json.dumps(_noop()))
            return

        input_data = _read_hook_input()
        if input_data is None:
            print(json.dumps(_noop()))
            return

        target = _scan_target(input_data)
        if target is None:
            print(json.dumps(_noop()))
            return
        event_name, source, text = target

        scan_result = _scan_pii(input_data, text, source)
        if scan_result is None:
            print(json.dumps(_noop()))
            return
        validated = _validated_result(scan_result)
        if validated is None:
            print(json.dumps(_noop()))
            return
        verdict, findings = validated
        output = (
            _noop()
            if verdict == "pass"
            else _decision(
                input_data,
                event_name,
                verdict,
                findings,
                summary=scan_result.get("summary"),
            )
        )
        print(json.dumps(output, ensure_ascii=False))
    except Exception:  # noqa: BLE001 - hook failures must remain silent and fail-open
        print(json.dumps(_noop()))


if __name__ == "__main__":
    main()
