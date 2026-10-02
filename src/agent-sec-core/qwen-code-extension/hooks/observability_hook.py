#!/usr/bin/env python3
"""Map Qwen Code hook input to agent-sec-core observability records.

This command hook is fail-open. It never returns a Qwen Code decision and
never forwards an unredacted sensitive metric when PII redaction fails.
"""

import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from typing import Any

from hook_config import env_flag_enabled
from pii_text import json_dumps as _json_dumps
from pii_text import text_sha256, value_to_text
from trace_context import trace_context, with_trace_context

_DEFAULT_CLI_TIMEOUT_SECONDS = 5
_MAX_CLI_TIMEOUT_SECONDS = 5


def _read_cli_timeout_seconds() -> int:
    try:
        timeout = int(
            os.environ.get("OBSERVABILITY_TIMEOUT", _DEFAULT_CLI_TIMEOUT_SECONDS)
        )
    except (TypeError, ValueError):
        return _DEFAULT_CLI_TIMEOUT_SECONDS
    if timeout <= 0:
        return _DEFAULT_CLI_TIMEOUT_SECONDS
    return min(timeout, _MAX_CLI_TIMEOUT_SECONDS)


_CLI_TIMEOUT_SECONDS = _read_cli_timeout_seconds()
_HOOK_ENABLED = env_flag_enabled("OBSERVABILITY_HOOK_ENABLED", True)
# Read one extra byte below to distinguish an exact-limit payload from truncation.
_MAX_PAYLOAD_SIZE = 1024 * 1024
# Qwen Code HookInput does not currently expose one stable run identifier across
# prompt, tool, and stop events. Use the zero GUID deliberately until reliable
# run correlation can be implemented from the hook protocol itself.
_ZERO_RUN_ID = "00000000-0000-0000-0000-000000000000"
_OBSERVABILITY_COMMAND = [
    "agent-sec-cli",
    "observability",
    "record",
    "--format",
    "json",
    "--stdin",
]
_PII_REDACT_COMMAND = [
    "agent-sec-cli",
    "scan-pii",
    "--stdin",
    "--format",
    "json",
    "--redact-output",
    "--source",
    "observability",
]
_SENSITIVE_METRIC_KEYS = {
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


def _noop() -> str:
    """Return an empty Qwen Code HookOutput JSON string."""
    return json.dumps({})


def _json_size_bytes(value: Any) -> int:
    return len(_json_dumps(value).encode("utf-8"))


def _pii_scan_input_sha256(value: Any) -> str | None:
    text = value_to_text(value)
    if not text.strip():
        return None
    return text_sha256(text)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _metadata(
    input_data: dict[str, Any], *, needs_tool_call_id: bool = False
) -> dict[str, Any] | None:
    context = trace_context(input_data)
    session_id = context.get("session_id")
    if session_id is None:
        return None

    metadata = {
        "sessionId": session_id,
        "runId": _ZERO_RUN_ID,
    }
    if needs_tool_call_id:
        tool_call_id = context.get("tool_call_id")
        if tool_call_id is None:
            return None
        metadata["toolCallId"] = tool_call_id
    return metadata


def _observed_at(input_data: dict[str, Any]) -> str:
    timestamp = input_data.get("timestamp")
    if isinstance(timestamp, str) and timestamp:
        return timestamp
    return _now_iso()


def _base_record(
    input_data: dict[str, Any],
    *,
    hook: str,
    metrics: dict[str, Any],
    needs_tool_call_id: bool = False,
) -> dict[str, Any] | None:
    if not metrics:
        return None
    metadata = _metadata(input_data, needs_tool_call_id=needs_tool_call_id)
    if metadata is None:
        return None
    return {
        "hook": hook,
        "observedAt": _observed_at(input_data),
        "metadata": metadata,
        "metrics": metrics,
    }


def _diagnostic(message: str) -> None:
    print(f"qwen-observability-hook: {message}", file=sys.stderr)


def _read_stdin_payload() -> str | bytes | None:
    """Read one bounded hook payload, returning None when it exceeds the limit."""
    stream = getattr(sys.stdin, "buffer", sys.stdin)
    payload = stream.read(_MAX_PAYLOAD_SIZE + 1)
    if len(payload) > _MAX_PAYLOAD_SIZE:
        _diagnostic(f"hook input exceeds {_MAX_PAYLOAD_SIZE} bytes; skipping event")
        return None
    return payload


def _process_text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace").strip()
    return str(value).strip()


def _process_output_details(*values: Any) -> str:
    details = "\n".join(part for value in values if (part := _process_text(value)))
    return details or "no stderr or stdout was captured"


def _redact_text(text: str, trace_input: dict[str, Any]) -> str | None:
    try:
        result = subprocess.run(
            with_trace_context(_PII_REDACT_COMMAND, trace_input),
            input=text,
            capture_output=True,
            text=True,
            timeout=_CLI_TIMEOUT_SECONDS,
            check=False,
        )
    except Exception:
        return None

    if result.returncode != 0:
        return None

    try:
        data = json.loads(result.stdout)
    except (json.JSONDecodeError, ValueError):
        return None
    if not isinstance(data, dict):
        return None

    redacted = data.get("redacted_text")
    return redacted if isinstance(redacted, str) else None


def _redact_sensitive_value(value: Any, trace_input: dict[str, Any]) -> Any:
    """Redact a sensitive metric value, or return _DROP on scan failure."""
    if isinstance(value, str):
        redacted = _redact_text(value, trace_input)
        return _DROP if redacted is None else redacted

    redacted = _redact_text(_json_dumps(value), trace_input)
    if redacted is None:
        return _DROP
    try:
        return json.loads(redacted)
    except (json.JSONDecodeError, ValueError):
        return redacted


def _redact_metrics(value: Any, trace_input: dict[str, Any]) -> Any:
    if isinstance(value, dict):
        redacted: dict[str, Any] = {}
        for key, item in value.items():
            safe_item = (
                _redact_sensitive_value(item, trace_input)
                if key in _SENSITIVE_METRIC_KEYS
                else _redact_metrics(item, trace_input)
            )
            if safe_item is not _DROP:
                redacted[key] = safe_item
        return redacted
    if isinstance(value, list):
        return [
            item
            for item in (_redact_metrics(item, trace_input) for item in value)
            if item is not _DROP
        ]
    return value


def _redact_observability_record(record: dict[str, Any]) -> dict[str, Any]:
    safe_record = dict(record)
    metadata = record.get("metadata")
    trace_input: dict[str, Any] = {}
    if isinstance(metadata, dict):
        trace_input = {
            "session_id": metadata.get("sessionId"),
            "tool_call_id": metadata.get("toolCallId"),
        }
    metrics = safe_record.get("metrics")
    if isinstance(metrics, dict):
        safe_record["metrics"] = _redact_metrics(metrics, trace_input)
    return safe_record


def _build_user_prompt_submit(input_data: dict[str, Any]) -> dict[str, Any] | None:
    prompt = input_data.get("prompt")
    # Qwen serializes a function-response-only tool re-entry as an empty prompt.
    if not isinstance(prompt, str) or not prompt.strip():
        return None

    metrics: dict[str, Any] = {
        "prompt": prompt,
        "user_input": prompt,
    }
    pii_hash = _pii_scan_input_sha256(prompt)
    if pii_hash is not None:
        metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(input_data, hook="before_agent_run", metrics=metrics)


def _build_pre_tool_use(input_data: dict[str, Any]) -> dict[str, Any] | None:
    metrics: dict[str, Any] = {}
    if "tool_name" in input_data:
        metrics["tool_name"] = input_data["tool_name"]
    if "tool_input" in input_data:
        tool_input = input_data["tool_input"]
        metrics["parameters"] = tool_input
        pii_hash = _pii_scan_input_sha256(tool_input)
        if pii_hash is not None:
            metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(
        input_data,
        hook="before_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _extract_exit_code(value: Any) -> Any | None:
    if not isinstance(value, dict):
        return None
    if "exit_code" in value:
        return value["exit_code"]
    if "exitCode" in value:
        return value["exitCode"]
    for key in ("returnDisplay", "llmContent"):
        nested = _extract_exit_code(value.get(key))
        if nested is not None:
            return nested
    return None


def _build_post_tool_use(input_data: dict[str, Any]) -> dict[str, Any] | None:
    metrics: dict[str, Any] = {"status": "success"}
    if "tool_response" in input_data:
        tool_response = input_data["tool_response"]
        metrics["result"] = tool_response
        metrics["result_size_bytes"] = _json_size_bytes(tool_response)
        pii_hash = _pii_scan_input_sha256(tool_response)
        if pii_hash is not None:
            metrics["pii_scan_input_sha256"] = pii_hash
        exit_code = _extract_exit_code(tool_response)
        if exit_code is not None:
            metrics["exit_code"] = exit_code
    return _base_record(
        input_data,
        hook="after_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _build_post_tool_use_failure(input_data: dict[str, Any]) -> dict[str, Any] | None:
    metrics: dict[str, Any] = {
        "status": "interrupted" if input_data.get("is_interrupt") is True else "error"
    }
    if "error" in input_data:
        error = input_data["error"]
        metrics["error"] = error
        pii_hash = _pii_scan_input_sha256(error)
        if pii_hash is not None:
            metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(
        input_data,
        hook="after_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _build_stop(input_data: dict[str, Any]) -> dict[str, Any] | None:
    response = input_data.get("last_assistant_message", "")
    has_text = bool(response)
    metrics: dict[str, Any] = {
        "response": response,
        "output_kind": "text" if has_text else "empty",
        "assistant_texts_count": 1 if has_text else 0,
        "success": True,
    }
    return _base_record(input_data, hook="after_agent_run", metrics=metrics)


def _build_stop_failure(input_data: dict[str, Any]) -> dict[str, Any] | None:
    error = input_data.get("error_details") or input_data.get("error") or "unknown"
    response = input_data.get("last_assistant_message", "")
    metrics: dict[str, Any] = {
        "error": error,
        "output_kind": "text" if response else "empty",
        "success": False,
    }
    if response:
        metrics["response"] = response
        metrics["assistant_texts_count"] = 1
    if input_data.get("error"):
        metrics["stop_reason"] = input_data["error"]
    return _base_record(input_data, hook="after_agent_run", metrics=metrics)


_BUILDERS = {
    "UserPromptSubmit": _build_user_prompt_submit,
    "PreToolUse": _build_pre_tool_use,
    "PostToolUse": _build_post_tool_use,
    "PostToolUseFailure": _build_post_tool_use_failure,
    "Stop": _build_stop,
    "StopFailure": _build_stop_failure,
}


def _build_record(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map a Qwen Code hook input to one observability record payload."""
    if not isinstance(input_data, dict):
        return None
    builder = _BUILDERS.get(input_data.get("hook_event_name"))
    if builder is None:
        return None
    return builder(input_data)


def _diagnostic_event_name(input_data: Any) -> str:
    if not isinstance(input_data, dict):
        return "unknown"
    event_name = input_data.get("hook_event_name")
    return event_name if event_name in _BUILDERS else "unknown"


def _record_observability(record: dict[str, Any]) -> None:
    record = _redact_observability_record(record)
    if not record.get("metrics"):
        return
    try:
        result = subprocess.run(
            _OBSERVABILITY_COMMAND,
            input=json.dumps(record, ensure_ascii=False),
            capture_output=True,
            text=True,
            timeout=_CLI_TIMEOUT_SECONDS,
            check=False,
        )
    except FileNotFoundError:
        _diagnostic(
            "agent-sec-cli executable was not found; install agent-sec-cli or add it to PATH"
        )
        return
    except subprocess.TimeoutExpired as exc:
        details = _process_output_details(exc.stderr, exc.stdout)
        _diagnostic(
            "agent-sec-cli observability record timed out "
            f"after {exc.timeout} seconds: {details}"
        )
        return
    except OSError as exc:
        _diagnostic(f"failed to start agent-sec-cli observability record: {exc}")
        return

    if result.returncode != 0:
        details = _process_output_details(
            getattr(result, "stderr", None), getattr(result, "stdout", None)
        )
        _diagnostic(
            "agent-sec-cli observability record failed "
            f"with exit code {result.returncode}: {details}"
        )


def main() -> None:
    if not _HOOK_ENABLED:
        print(_noop())
        return

    try:
        payload = _read_stdin_payload()
        if payload is None:
            print(_noop())
            return
        input_data = json.loads(payload)
    except (json.JSONDecodeError, EOFError, OSError, ValueError):
        print(_noop())
        return

    try:
        record = _build_record(input_data)
        if record is not None:
            _record_observability(record)
    except Exception as exc:
        _diagnostic(
            f"unexpected {type(exc).__name__} while processing "
            f"{_diagnostic_event_name(input_data)}"
        )
    print(_noop())


if __name__ == "__main__":
    main()
