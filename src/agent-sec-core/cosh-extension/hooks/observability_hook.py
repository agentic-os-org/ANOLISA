#!/usr/bin/env python3
"""Cosh hook that records current hook input as observability metrics.

The hook is intentionally self-contained. It reads a single cosh hook JSON
payload from stdin, maps only fields present in that payload, sends one
``agent-sec-cli observability record`` payload, and emits no run decision.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from typing import Any

from hook_config import env_flag_enabled
from pii_text import json_dumps as _json_dumps
from pii_text import text_sha256, value_to_text

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
    """Return an empty cosh HookOutput JSON string."""
    return json.dumps({})


def _json_loads(value: str) -> Any:
    return json.loads(value)


def _json_size_bytes(value: Any) -> int:
    return len(_json_dumps(value).encode("utf-8"))


def _pii_scan_input_sha256(value: Any) -> str | None:
    text = value_to_text(value)
    if not text.strip():
        return None
    return text_sha256(text)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _string_or_empty(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return str(value)


def _synthetic_id(kind: str, input_data: dict[str, Any]) -> str:
    digest = hashlib.sha256(_json_dumps(input_data).encode("utf-8")).hexdigest()[:16]
    return f"synthetic-{kind}-{digest}"


def _metadata(
    input_data: dict[str, Any], *, needs_tool_call_id: bool = False
) -> dict[str, Any]:
    metadata = {
        "sessionId": _string_or_empty(input_data.get("session_id")),
        "runId": _string_or_empty(input_data.get("run_id"))
        or _synthetic_id("run", input_data),
    }
    if needs_tool_call_id:
        metadata["toolCallId"] = _string_or_empty(
            input_data.get("tool_use_id") or input_data.get("toolCallId")
        ) or _synthetic_id("tool", input_data)
    return metadata


def _observed_at(input_data: dict[str, Any]) -> str:
    timestamp = input_data.get("timestamp")
    if isinstance(timestamp, str) and timestamp:
        return timestamp
    return _now_iso()


def _message_content(message: Any) -> Any:
    if isinstance(message, dict) and "content" in message:
        return message["content"]
    return message


def _system_messages(messages: list[Any]) -> list[Any]:
    return [
        _message_content(message)
        for message in messages
        if isinstance(message, dict) and message.get("role") == "system"
    ]


def _last_user_message(messages: list[Any]) -> Any | None:
    for message in reversed(messages):
        if isinstance(message, dict) and message.get("role") == "user":
            return _message_content(message)
    return None


def _first_candidate_finish_reason(llm_response: dict[str, Any]) -> Any | None:
    candidates = llm_response.get("candidates")
    if not isinstance(candidates, list) or not candidates:
        return None
    first = candidates[0]
    if isinstance(first, dict) and "finishReason" in first:
        return first["finishReason"]
    return None


def _assistant_texts_count(llm_response: dict[str, Any]) -> int:
    candidates = llm_response.get("candidates")
    if isinstance(candidates, list):
        count = 0
        for candidate in candidates:
            if not isinstance(candidate, dict):
                continue
            content = candidate.get("content")
            if not isinstance(content, dict):
                continue
            parts = content.get("parts")
            if isinstance(parts, str):
                count += 1
            elif isinstance(parts, list):
                count += sum(
                    1
                    for part in parts
                    if isinstance(part, str)
                    or (isinstance(part, dict) and isinstance(part.get("text"), str))
                )
        return count
    return 1 if llm_response.get("text") else 0


def _base_record(
    input_data: dict[str, Any],
    *,
    hook: str,
    metrics: dict[str, Any],
    needs_tool_call_id: bool = False,
) -> dict[str, Any] | None:
    if not metrics:
        return None
    return {
        "hook": hook,
        "observedAt": _observed_at(input_data),
        "metadata": _metadata(input_data, needs_tool_call_id=needs_tool_call_id),
        "metrics": metrics,
    }


def _diagnostic(message: str) -> None:
    print(f"observability-hook: {message}", file=sys.stderr)


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
    if details:
        return details
    return "no stderr or stdout was captured"


def _redact_text(text: str) -> str | None:
    try:
        result = subprocess.run(
            _PII_REDACT_COMMAND,
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
        data = _json_loads(result.stdout)
    except (json.JSONDecodeError, ValueError):
        return None
    if not isinstance(data, dict):
        return None

    redacted = data.get("redacted_text")
    return redacted if isinstance(redacted, str) else None


def _redact_sensitive_value(value: Any) -> Any:
    """Redact a sensitive metric value, or return _DROP on scan failure."""
    if isinstance(value, str):
        redacted = _redact_text(value)
        return _DROP if redacted is None else redacted

    serialized = _json_dumps(value)
    redacted = _redact_text(serialized)
    if redacted is None:
        return _DROP
    try:
        return _json_loads(redacted)
    except (json.JSONDecodeError, ValueError):
        return redacted


def _redact_metrics(value: Any) -> Any:
    if isinstance(value, dict):
        redacted: dict[str, Any] = {}
        for key, item in value.items():
            if key in _SENSITIVE_METRIC_KEYS:
                safe_item = _redact_sensitive_value(item)
            else:
                safe_item = _redact_metrics(item)
            if safe_item is not _DROP:
                redacted[key] = safe_item
        return redacted
    if isinstance(value, list):
        return [
            item
            for item in (_redact_metrics(item) for item in value)
            if item is not _DROP
        ]
    return value


def _redact_observability_record(record: dict[str, Any]) -> dict[str, Any]:
    safe_record = dict(record)
    metrics = safe_record.get("metrics")
    if isinstance(metrics, dict):
        safe_record["metrics"] = _redact_metrics(metrics)
    return safe_record


def _build_user_prompt_submit(input_data: dict[str, Any]) -> dict[str, Any] | None:
    metrics: dict[str, Any] = {}
    if "prompt" in input_data:
        prompt = input_data["prompt"]
        metrics["prompt"] = prompt
        metrics["user_input"] = prompt
        pii_hash = _pii_scan_input_sha256(prompt) if isinstance(prompt, str) else None
        if pii_hash is not None:
            metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(input_data, hook="before_agent_run", metrics=metrics)


def _build_before_model(input_data: dict[str, Any]) -> dict[str, Any] | None:
    llm_request = input_data.get("llm_request")
    if not isinstance(llm_request, dict):
        llm_request = {}

    metrics: dict[str, Any] = {}
    messages = llm_request.get("messages")
    if isinstance(messages, list):
        metrics["prompt"] = messages
        system_messages = _system_messages(messages)
        if system_messages:
            metrics["system_prompt"] = system_messages
        last_user_message = _last_user_message(messages)
        if last_user_message is not None:
            metrics["user_input"] = last_user_message
        metrics["history_messages_count"] = len(messages)
    if "model" in llm_request:
        metrics["model_id"] = llm_request["model"]
    return _base_record(input_data, hook="before_llm_call", metrics=metrics)


def _build_after_model(input_data: dict[str, Any]) -> dict[str, Any] | None:
    llm_request = input_data.get("llm_request")
    if not isinstance(llm_request, dict):
        llm_request = {}
    llm_response = input_data.get("llm_response")
    if not isinstance(llm_response, dict):
        llm_response = {}

    metrics: dict[str, Any] = {"outcome": "success"}
    if "text" in llm_response:
        metrics["response"] = llm_response["text"]
    finish_reason = _first_candidate_finish_reason(llm_response)
    if finish_reason is not None:
        metrics["stop_reason"] = finish_reason
    metrics["assistant_texts_count"] = _assistant_texts_count(llm_response)
    if "llm_request" in input_data:
        metrics["request_payload_bytes"] = _json_size_bytes(llm_request)
    if "llm_response" in input_data:
        metrics["response_stream_bytes"] = _json_size_bytes(llm_response)
    return _base_record(input_data, hook="after_llm_call", metrics=metrics)


def _build_pre_tool_use(input_data: dict[str, Any]) -> dict[str, Any] | None:
    metrics: dict[str, Any] = {}
    if "tool_name" in input_data:
        metrics["tool_name"] = input_data["tool_name"]
    if "tool_input" in input_data:
        metrics["parameters"] = input_data["tool_input"]
    return _base_record(
        input_data,
        hook="before_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _build_post_tool_use(input_data: dict[str, Any]) -> dict[str, Any] | None:
    metrics: dict[str, Any] = {"status": "success"}
    if "tool_response" in input_data:
        tool_response = input_data["tool_response"]
        metrics["result"] = tool_response
        pii_hash = _pii_scan_input_sha256(tool_response)
        if pii_hash is not None:
            metrics["pii_scan_input_sha256"] = pii_hash
        metrics["result_size_bytes"] = _json_size_bytes(tool_response)
        if isinstance(tool_response, dict):
            if "exit_code" in tool_response:
                metrics["exit_code"] = tool_response["exit_code"]
            elif "exitCode" in tool_response:
                metrics["exit_code"] = tool_response["exitCode"]
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
        metrics["error"] = input_data["error"]
        pii_hash = _pii_scan_input_sha256(input_data["error"])
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
    metrics = {
        "response": response,
        "output_kind": "text" if has_text else "empty",
        "assistant_texts_count": 1 if has_text else 0,
        "success": True,
    }
    return _base_record(input_data, hook="after_agent_run", metrics=metrics)


_BUILDERS = {
    "UserPromptSubmit": _build_user_prompt_submit,
    "BeforeModel": _build_before_model,
    "AfterModel": _build_after_model,
    "PreToolUse": _build_pre_tool_use,
    "PostToolUse": _build_post_tool_use,
    "PostToolUseFailure": _build_post_tool_use_failure,
    "Stop": _build_stop,
}


def _build_record(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map a cosh hook input to one observability record payload."""
    if not isinstance(input_data, dict):
        return None
    builder = _BUILDERS.get(input_data.get("hook_event_name"))
    if builder is None:
        return None
    return builder(input_data)


def _record_observability(record: dict[str, Any]) -> None:
    record = _redact_observability_record(record)
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
            "agent-sec-cli executable was not found; "
            "install agent-sec-cli or add it to PATH"
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
    except Exception:
        pass
    print(_noop())


if __name__ == "__main__":
    main()
