"""Subprocess wrapper for calling agent-sec-cli — fail-open, never raises."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from typing import Any

from .pii_text import json_dumps as _json_dumps

_OBSERVABILITY_SENSITIVE_KEYS = {
    "prompt",
    "user_input",
    "system_prompt",
    "messages",
    "response",
    "parameters",
    "result",
    "error",
    "tool_calls",
}
_DROP = object()


@dataclass
class CliResult:
    """Result of an agent-sec-cli subprocess invocation."""

    stdout: str
    stderr: str
    exit_code: int


def call_agent_sec_cli(
    args: list[str],
    timeout: float = 10.0,
    stdin: str | None = None,
    trace_context: dict[str, str] | None = None,
) -> CliResult:
    """Call agent-sec-cli as a subprocess.

    - Never raises exceptions (fail-open principle)
    - On timeout → CliResult("", "timed out", 124)
    - On other errors → CliResult("", str(e), 1)
    """
    final_args = _with_trace_context(args, trace_context)
    try:
        proc = subprocess.run(
            ["agent-sec-cli", *final_args],
            input=stdin,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return CliResult(
            stdout=proc.stdout,
            stderr=proc.stderr,
            exit_code=proc.returncode,
        )
    except subprocess.TimeoutExpired:
        return CliResult(stdout="", stderr="timed out", exit_code=124)
    except Exception as e:
        return CliResult(stdout="", stderr=str(e), exit_code=1)


_TRACE_FIELD_SPECS = (
    ("trace_id", ("trace_id", "traceId")),
    ("session_id", ("session_id", "sessionId")),
    ("run_id", ("run_id", "runId")),
    ("call_id", ("call_id", "callId")),
    ("tool_call_id", ("tool_call_id", "toolCallId", "tool_use_id", "toolUseId")),
)


def trace_context(data: dict[str, Any]) -> dict[str, str]:
    """Build agent-sec-cli trace context from Hermes hook kwargs."""
    context: dict[str, str] = {"agent_name": "hermes"}
    for output_key, input_keys in _TRACE_FIELD_SPECS:
        for input_key in input_keys:
            value = data.get(input_key)
            if isinstance(value, str) and value.strip():
                context[output_key] = value.strip()
                break
    return context


def _redact_text_for_observability(text: str, timeout: float) -> str | None:
    result = call_agent_sec_cli(
        [
            "scan-pii",
            "--stdin",
            "--format",
            "json",
            "--redact-output",
            "--source",
            "observability",
        ],
        timeout=timeout,
        stdin=text,
    )
    if result.exit_code != 0:
        return None
    try:
        data = json.loads(result.stdout)
    except (json.JSONDecodeError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    redacted_text = data.get("redacted_text")
    return redacted_text if isinstance(redacted_text, str) else None


def _redact_sensitive_value(value: Any, timeout: float) -> Any:
    if isinstance(value, str):
        redacted = _redact_text_for_observability(value, timeout)
        return _DROP if redacted is None else redacted

    redacted = _redact_text_for_observability(_json_dumps(value), timeout)
    if redacted is None:
        return _DROP
    try:
        return json.loads(redacted)
    except (json.JSONDecodeError, ValueError):
        return redacted


def _redact_observability_value(value: Any, timeout: float) -> Any:
    if isinstance(value, dict):
        redacted: dict[str, Any] = {}
        for key, item in value.items():
            if key in _OBSERVABILITY_SENSITIVE_KEYS:
                safe_item = _redact_sensitive_value(item, timeout)
            else:
                safe_item = _redact_observability_value(item, timeout)
            if safe_item is not _DROP:
                redacted[key] = safe_item
        return redacted
    if isinstance(value, list):
        return [
            item
            for item in (_redact_observability_value(item, timeout) for item in value)
            if item is not _DROP
        ]
    return value


def _redact_observability_record(
    record: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    safe_record = dict(record)
    metrics = safe_record.get("metrics")
    if isinstance(metrics, dict):
        safe_record["metrics"] = _redact_observability_value(metrics, timeout)
    return safe_record


def _with_trace_context(
    args: list[str],
    context: dict[str, str] | None,
) -> list[str]:
    if not context:
        return args
    return [
        "--trace-context",
        json.dumps(context, ensure_ascii=False, separators=(",", ":")),
        *args,
    ]


def record_hermes_observability(
    record: dict[str, Any],
    timeout: float = 5.0,
) -> CliResult:
    """Emit one Hermes observability record via agent-sec-cli stdin."""
    safe_record = _redact_observability_record(record, timeout)
    return call_agent_sec_cli(
        ["observability", "record", "--format", "json", "--stdin"],
        timeout=timeout,
        stdin=json.dumps(safe_record, ensure_ascii=False, separators=(",", ":")),
    )
