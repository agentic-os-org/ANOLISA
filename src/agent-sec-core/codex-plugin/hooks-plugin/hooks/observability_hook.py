#!/usr/bin/env python3
"""Record Codex turn and tool lifecycle events as AgentSec observability data.

The hook is intentionally self-contained. It reads one Codex hook payload from
stdin, maps only host-provided fields, redacts sensitive metrics, and invokes
``agent-sec-cli observability record``. Every failure path is fail-open and the
hook always emits an empty JSON object so it cannot change Codex behavior.
"""

import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from typing import Any, Callable

from hook_config import env_flag_enabled

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
    """Return a Codex-compatible empty hook output."""
    return json.dumps({})


def _diagnostic(message: str) -> None:
    """Write a payload-free diagnostic without affecting hook output."""
    print(f"observability-hook: {message}", file=sys.stderr)


def _read_stdin_payload() -> str | bytes | None:
    """Read one bounded hook payload, returning ``None`` when it exceeds the limit."""
    stream = getattr(sys.stdin, "buffer", sys.stdin)
    payload = stream.read(_MAX_PAYLOAD_SIZE + 1)
    if len(payload) > _MAX_PAYLOAD_SIZE:
        _diagnostic(f"hook input exceeds {_MAX_PAYLOAD_SIZE} bytes; skipping event")
        return None
    return payload


def _json_dumps(value: Any) -> str:
    """Serialize hook values deterministically for hashing and subprocess input."""
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
        default=str,
    )


def _json_size_bytes(value: Any) -> int:
    """Return the UTF-8 size of a deterministic JSON representation."""
    return len(_json_dumps(value).encode("utf-8"))


def _value_to_text(value: Any) -> str:
    """Convert a hook value to the exact text hashed for PII correlation."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return _json_dumps(value)


def _pii_scan_input_sha256(value: Any) -> str | None:
    """Return the scan input hash used to correlate PII security events."""
    text = _value_to_text(value)
    if not text.strip():
        return None
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _now_iso() -> str:
    """Return a timezone-aware UTC timestamp in wire format."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _non_empty_string(value: Any) -> str | None:
    """Return a stripped non-empty string, otherwise ``None``."""
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None


def _metadata(
    input_data: dict[str, Any], *, needs_tool_call_id: bool = False
) -> dict[str, str] | None:
    """Build metadata from Codex IDs without manufacturing correlation values."""
    session_id = _non_empty_string(input_data.get("session_id"))
    turn_id = _non_empty_string(input_data.get("turn_id"))
    missing = []
    if session_id is None:
        missing.append("session_id")
    if turn_id is None:
        missing.append("turn_id")

    tool_use_id = None
    if needs_tool_call_id:
        tool_use_id = _non_empty_string(input_data.get("tool_use_id"))
        if tool_use_id is None:
            missing.append("tool_use_id")

    if missing:
        _diagnostic(
            f"skipping record with missing correlation field(s): {', '.join(missing)}"
        )
        return None

    metadata = {
        "sessionId": session_id,
        "runId": turn_id,
    }
    if tool_use_id is not None:
        metadata["toolCallId"] = tool_use_id
    return metadata


def _base_record(
    input_data: dict[str, Any],
    *,
    hook: str,
    metrics: dict[str, Any],
    needs_tool_call_id: bool = False,
) -> dict[str, Any] | None:
    """Build one schema-compatible record when metadata and metrics exist."""
    if not metrics:
        return None
    metadata = _metadata(input_data, needs_tool_call_id=needs_tool_call_id)
    if metadata is None:
        return None
    return {
        "hook": hook,
        "observedAt": _now_iso(),
        "metadata": metadata,
        "metrics": metrics,
    }


def _build_user_prompt_submit(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map ``UserPromptSubmit`` to ``before_agent_run``."""
    metrics: dict[str, Any] = {}
    if "prompt" in input_data:
        prompt = input_data["prompt"]
        metrics["prompt"] = prompt
        metrics["user_input"] = prompt
        pii_hash = _pii_scan_input_sha256(prompt)
        if pii_hash is not None:
            metrics["pii_scan_input_sha256"] = pii_hash

    model = _non_empty_string(input_data.get("model"))
    if model is not None:
        metrics["model_id"] = model

    return _base_record(input_data, hook="before_agent_run", metrics=metrics)


def _build_pre_tool_use(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map ``PreToolUse`` to ``before_tool_call``."""
    metrics: dict[str, Any] = {}
    tool_name = _non_empty_string(input_data.get("tool_name"))
    if tool_name is not None:
        metrics["tool_name"] = tool_name
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


def _extract_exit_code(tool_response: Any) -> Any:
    """Return a directly exposed tool exit code without guessing nested schemas."""
    if not isinstance(tool_response, dict):
        return None
    if "exit_code" in tool_response:
        return tool_response["exit_code"]
    if "exitCode" in tool_response:
        return tool_response["exitCode"]
    return None


def _exit_status(exit_code: Any) -> str | None:
    """Derive status only when the host exposes an integer exit code."""
    if isinstance(exit_code, bool) or not isinstance(exit_code, int):
        return None
    return "success" if exit_code == 0 else "error"


def _build_post_tool_use(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map ``PostToolUse`` to ``after_tool_call``."""
    metrics: dict[str, Any] = {}
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
        status = _exit_status(exit_code)
        if status is not None:
            metrics["status"] = status

    return _base_record(
        input_data,
        hook="after_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _build_stop(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map ``Stop`` to ``after_agent_run`` without inventing unavailable metrics."""
    metrics: dict[str, Any] = {}
    if "last_assistant_message" in input_data:
        response = input_data["last_assistant_message"]
        metrics["response"] = response
        has_text = isinstance(response, str) and bool(response)
        metrics["output_kind"] = "text" if has_text else "empty"
        metrics["assistant_texts_count"] = 1 if has_text else 0

    model = _non_empty_string(input_data.get("model"))
    if model is not None:
        metrics["final_model_id"] = model

    return _base_record(input_data, hook="after_agent_run", metrics=metrics)


_RecordBuilder = Callable[[dict[str, Any]], dict[str, Any] | None]
_BUILDERS: dict[str, _RecordBuilder] = {
    "UserPromptSubmit": _build_user_prompt_submit,
    "PreToolUse": _build_pre_tool_use,
    "PostToolUse": _build_post_tool_use,
    "Stop": _build_stop,
}


def _build_record(input_data: dict[str, Any]) -> dict[str, Any] | None:
    """Map one supported Codex hook payload to an observability record."""
    if not isinstance(input_data, dict):
        return None
    builder = _BUILDERS.get(input_data.get("hook_event_name"))
    if builder is None:
        return None
    return builder(input_data)


def _process_text(value: Any) -> str:
    """Normalize subprocess output for bounded diagnostics."""
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace").strip()
    return str(value).strip()


def _process_output_details(*values: Any) -> str:
    """Return subprocess details without including hook payloads."""
    details = "\n".join(part for value in values if (part := _process_text(value)))
    if details:
        return details
    return "no stderr or stdout was captured"


def _record_trace_context(record: dict[str, Any]) -> dict[str, str]:
    """Build AgentSec trace context from validated observability metadata."""
    context = {"agent_name": "codex"}
    metadata = record.get("metadata")
    if not isinstance(metadata, dict):
        return context

    field_map = {
        "session_id": "sessionId",
        "run_id": "runId",
        "tool_call_id": "toolCallId",
    }
    for output_key, input_key in field_map.items():
        value = _non_empty_string(metadata.get(input_key))
        if value is not None:
            context[output_key] = value
    return context


def _redact_text(text: str, security_trace_context: dict[str, str]) -> str | None:
    """Return locally redacted text, or ``None`` when redaction fails."""
    command = [
        _PII_REDACT_COMMAND[0],
        "--trace-context",
        _json_dumps(security_trace_context),
        *_PII_REDACT_COMMAND[1:],
    ]
    try:
        result = subprocess.run(
            command,
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


def _redact_sensitive_value(
    value: Any,
    cache: dict[str, Any],
    security_trace_context: dict[str, str],
) -> Any:
    """Redact one sensitive value, returning ``_DROP`` on any scan failure."""
    cache_key = f"{type(value).__name__}:{_json_dumps(value)}"
    if cache_key in cache:
        return cache[cache_key]

    if isinstance(value, str):
        safe_value: Any = _redact_text(value, security_trace_context)
        if safe_value is None:
            safe_value = _DROP
    else:
        redacted = _redact_text(_json_dumps(value), security_trace_context)
        if redacted is None:
            safe_value = _DROP
        else:
            try:
                safe_value = json.loads(redacted)
            except (json.JSONDecodeError, ValueError):
                safe_value = redacted

    cache[cache_key] = safe_value
    return safe_value


def _redact_metrics(
    value: Any,
    cache: dict[str, Any],
    security_trace_context: dict[str, str],
) -> Any:
    """Recursively redact allowlisted sensitive metric fields."""
    if isinstance(value, dict):
        redacted: dict[str, Any] = {}
        for key, item in value.items():
            if key in _SENSITIVE_METRIC_KEYS:
                safe_item = _redact_sensitive_value(
                    item,
                    cache,
                    security_trace_context,
                )
            else:
                safe_item = _redact_metrics(item, cache, security_trace_context)
            if safe_item is not _DROP:
                redacted[key] = safe_item
        return redacted
    if isinstance(value, list):
        return [
            item
            for item in (
                _redact_metrics(item, cache, security_trace_context) for item in value
            )
            if item is not _DROP
        ]
    return value


def _redact_observability_record(record: dict[str, Any]) -> dict[str, Any]:
    """Return a record whose sensitive metrics are redacted or removed."""
    safe_record = dict(record)
    metrics = safe_record.get("metrics")
    if isinstance(metrics, dict):
        safe_record["metrics"] = _redact_metrics(
            metrics,
            {},
            _record_trace_context(record),
        )
    return safe_record


def _record_observability(record: dict[str, Any]) -> None:
    """Redact and persist one record through the public AgentSec CLI."""
    safe_record = _redact_observability_record(record)
    metrics = safe_record.get("metrics")
    if not isinstance(metrics, dict) or not metrics:
        _diagnostic("skipping record because no safe metrics remain after redaction")
        return

    try:
        result = subprocess.run(
            _OBSERVABILITY_COMMAND,
            input=_json_dumps(safe_record),
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
    """Read one Codex hook event, record it best-effort, and always continue."""
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
        _diagnostic(f"unexpected {type(exc).__name__}; record skipped")
    print(_noop())


if __name__ == "__main__":
    main()
