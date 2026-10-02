#!/usr/bin/env python3
"""Record Qoder agent and tool lifecycle events as AgentSec observability data.

Qoder does not expose model-call lifecycle events, so this hook deliberately
maps only agent-run and tool-call events. The hook is fail-open and never emits
a Qoder decision. Sensitive metrics are persisted only after local PII
redaction succeeds.
"""

import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from typing import Any, Callable

from qoder_hook_common import env_flag_enabled, trace_context

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
_MAX_PAYLOAD_SIZE = 1024 * 1024
# Qoder does not expose a stable run identifier across its hook events. Match
# the Hermes/Qwen adapters and use a schema-compatible zero GUID rather than
# inventing plugin-owned correlation state.
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
    "--include-low-confidence",
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


def _diagnostic(message: str) -> None:
    """Write a payload-free diagnostic without affecting Qoder execution."""
    try:
        print(f"qoder-observability-hook: {message}", file=sys.stderr)
    except Exception:  # noqa: BLE001 - diagnostics must preserve fail-open behavior
        pass


def _read_stdin_payload() -> str | bytes | None:
    """Read one bounded hook payload, returning ``None`` when oversized."""
    stream = getattr(sys.stdin, "buffer", sys.stdin)
    payload = stream.read(_MAX_PAYLOAD_SIZE + 1)
    if len(payload) > _MAX_PAYLOAD_SIZE:
        _diagnostic(f"hook input exceeds {_MAX_PAYLOAD_SIZE} bytes; skipping event")
        return None
    return payload


def _json_dumps(value: Any) -> str:
    """Serialize values deterministically for subprocess input and hashing."""
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
        default=str,
    )


def _json_size_bytes(value: Any) -> int:
    """Return the UTF-8 size of one deterministic JSON representation."""
    return len(_json_dumps(value).encode("utf-8"))


def _value_to_text(value: Any) -> str:
    """Convert a hook value to the exact text sent to PII scanning."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return _json_dumps(value)


def _pii_scan_input_sha256(value: Any) -> str | None:
    """Return the hash used to correlate a redaction PII security event."""
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


def _observability_metadata(
    context: dict[str, str], *, needs_tool_call_id: bool = False
) -> dict[str, str] | None:
    """Build metadata accepted by the current AgentSec schema."""
    session_id = _non_empty_string(context.get("session_id"))
    if session_id is None:
        return None

    metadata = {
        "sessionId": session_id,
        "runId": _ZERO_RUN_ID,
    }
    if needs_tool_call_id:
        tool_call_id = _non_empty_string(context.get("tool_call_id"))
        if tool_call_id is None:
            return None
        metadata["toolCallId"] = tool_call_id
        call_id = _non_empty_string(context.get("call_id"))
        if call_id is not None:
            metadata["callId"] = call_id
    return metadata


def _with_resolved_trace_context(args: list[str], context: dict[str, str]) -> list[str]:
    """Prepend one host-derived trace context to an AgentSec CLI command."""
    return [
        args[0],
        "--trace-context",
        json.dumps(context, ensure_ascii=False, separators=(",", ":"), sort_keys=True),
        *args[1:],
    ]


def _base_record(
    context: dict[str, str],
    *,
    hook: str,
    metrics: dict[str, Any],
    needs_tool_call_id: bool = False,
) -> dict[str, Any] | None:
    """Build one record accepted by the current AgentSec schema."""
    if not metrics:
        return None
    metadata = _observability_metadata(context, needs_tool_call_id=needs_tool_call_id)
    if metadata is None:
        return None
    return {
        "hook": hook,
        "observedAt": _now_iso(),
        "metadata": metadata,
        "metrics": metrics,
    }


def _build_user_prompt_submit(
    input_data: dict[str, Any], context: dict[str, str]
) -> dict[str, Any] | None:
    """Map ``UserPromptSubmit`` to ``before_agent_run``."""
    prompt = input_data.get("prompt")
    if not isinstance(prompt, str) or not prompt.strip():
        return None
    metrics: dict[str, Any] = {
        "prompt": prompt,
        "user_input": prompt,
    }
    pii_hash = _pii_scan_input_sha256(prompt)
    if pii_hash is not None:
        metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(
        context,
        hook="before_agent_run",
        metrics=metrics,
    )


def _build_pre_tool_use(
    input_data: dict[str, Any], context: dict[str, str]
) -> dict[str, Any] | None:
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
        context,
        hook="before_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _extract_exit_code(tool_response: Any) -> Any:
    """Return a directly exposed tool exit code without guessing response shapes."""
    if not isinstance(tool_response, dict):
        return None
    if "exit_code" in tool_response:
        return tool_response["exit_code"]
    if "exitCode" in tool_response:
        return tool_response["exitCode"]
    return None


def _exit_status(exit_code: Any) -> str | None:
    """Derive a status only when Qoder exposes an integer exit code."""
    if isinstance(exit_code, bool) or not isinstance(exit_code, int):
        return None
    return "success" if exit_code == 0 else "error"


def _build_post_tool_use(
    input_data: dict[str, Any], context: dict[str, str]
) -> dict[str, Any] | None:
    """Map successful ``PostToolUse`` to ``after_tool_call``."""
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
        status = _exit_status(exit_code)
        if status is not None:
            metrics["status"] = status
    return _base_record(
        context,
        hook="after_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _build_post_tool_use_failure(
    input_data: dict[str, Any], context: dict[str, str]
) -> dict[str, Any] | None:
    """Map ``PostToolUseFailure`` to a failed ``after_tool_call`` record."""
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
        context,
        hook="after_tool_call",
        metrics=metrics,
        needs_tool_call_id=True,
    )


def _build_stop(
    input_data: dict[str, Any], context: dict[str, str]
) -> dict[str, Any] | None:
    """Map ``Stop`` to a successful ``after_agent_run`` record."""
    response = input_data.get("last_assistant_message", "")
    has_text = isinstance(response, str) and bool(response)
    metrics: dict[str, Any] = {
        "response": response,
        "output_kind": "text" if has_text else "empty",
        "assistant_texts_count": 1 if has_text else 0,
        "success": True,
    }
    return _base_record(
        context,
        hook="after_agent_run",
        metrics=metrics,
    )


def _build_stop_failure(
    input_data: dict[str, Any], context: dict[str, str]
) -> dict[str, Any] | None:
    """Map ``StopFailure`` to a failed ``after_agent_run`` record."""
    response = input_data.get("last_assistant_message", "")
    has_text = isinstance(response, str) and bool(response)
    error_details = _non_empty_string(input_data.get("error_details"))
    error_category = _non_empty_string(input_data.get("error"))
    # The required error category identifies an API failure, not a model stop
    # reason. Use it only when Qoder omits the optional human-readable details.
    error = error_details or error_category or "unknown"
    metrics: dict[str, Any] = {
        "error": error,
        "output_kind": "text" if has_text else "empty",
        "assistant_texts_count": 1 if has_text else 0,
        "success": False,
    }
    if has_text:
        metrics["response"] = response
    return _base_record(
        context,
        hook="after_agent_run",
        metrics=metrics,
    )


_RecordBuilder = Callable[[dict[str, Any], dict[str, str]], dict[str, Any] | None]
_BUILDERS: dict[str, _RecordBuilder] = {
    "UserPromptSubmit": _build_user_prompt_submit,
    "PreToolUse": _build_pre_tool_use,
    "PostToolUse": _build_post_tool_use,
    "PostToolUseFailure": _build_post_tool_use_failure,
    "Stop": _build_stop,
    "StopFailure": _build_stop_failure,
}


def _build_record(
    input_data: dict[str, Any], context: dict[str, str] | None = None
) -> dict[str, Any] | None:
    """Map one supported Qoder payload to an observability record."""
    if not isinstance(input_data, dict):
        return None
    event_name = _non_empty_string(input_data.get("hook_event_name"))
    if event_name is None:
        return None
    builder = _BUILDERS.get(event_name)
    if builder is None:
        return None
    resolved = trace_context(input_data) if context is None else context
    return builder(input_data, resolved)


def _diagnostic_event_name(input_data: Any) -> str:
    """Return only a trusted event name for diagnostic output."""
    if not isinstance(input_data, dict):
        return "unknown"
    event_name = _non_empty_string(input_data.get("hook_event_name"))
    return event_name if event_name in _BUILDERS else "unknown"


def _unexpected_failure_diagnostic(exc: Exception, input_data: Any) -> None:
    """Report an unexpected failure without allowing reporting to raise."""
    try:
        _diagnostic(
            f"unexpected {type(exc).__name__} while processing "
            f"{_diagnostic_event_name(input_data)}"
        )
    except Exception:  # noqa: BLE001 - failure reporting is the final hook boundary
        pass


def _process_text(value: Any) -> str:
    """Normalize subprocess output for bounded diagnostics."""
    if value is None:
        return ""
    if isinstance(value, bytes):
        text = value.decode("utf-8", errors="replace").strip()
    else:
        text = str(value).strip()
    return text[:1000]


def _process_output_details(*values: Any) -> str:
    """Return subprocess details without including raw Qoder hook payloads."""
    details = "\n".join(part for value in values if (part := _process_text(value)))
    return details or "no stderr or stdout was captured"


def _redact_text(text: str, context: dict[str, str]) -> str | None:
    """Return locally redacted text, or ``None`` when redaction fails."""
    try:
        result = subprocess.run(
            _with_resolved_trace_context(_PII_REDACT_COMMAND, context),
            input=text,
            capture_output=True,
            text=True,
            timeout=_CLI_TIMEOUT_SECONDS,
            check=False,
        )
        if result.returncode != 0:
            return None
        data = json.loads(result.stdout)
        if not isinstance(data, dict):
            return None
        redacted = data.get("redacted_text")
        return redacted if isinstance(redacted, str) else None
    except Exception:  # noqa: BLE001 - any redaction failure must remain fail-open
        return None


def _redact_sensitive_value(
    value: Any,
    cache: dict[str, Any],
    context: dict[str, str],
) -> Any:
    """Redact one sensitive value, returning ``_DROP`` on scan failure."""
    cache_key = f"{type(value).__name__}:{_json_dumps(value)}"
    if cache_key in cache:
        return cache[cache_key]

    if isinstance(value, str):
        safe_value: Any = _redact_text(value, context)
        if safe_value is None:
            safe_value = _DROP
    else:
        redacted = _redact_text(_json_dumps(value), context)
        if redacted is None:
            safe_value = _DROP
        else:
            try:
                safe_value = json.loads(redacted)
            except (json.JSONDecodeError, TypeError, ValueError):
                safe_value = redacted
    if safe_value is _DROP:
        _diagnostic("PII redaction failed; sensitive metric dropped")
    cache[cache_key] = safe_value
    return safe_value


def _redact_metrics(
    value: Any,
    cache: dict[str, Any],
    context: dict[str, str],
) -> Any:
    """Recursively redact allowlisted sensitive metric fields."""
    if isinstance(value, dict):
        redacted: dict[str, Any] = {}
        for key, item in value.items():
            if key in _SENSITIVE_METRIC_KEYS:
                safe_item = _redact_sensitive_value(item, cache, context)
            else:
                safe_item = _redact_metrics(item, cache, context)
            if safe_item is not _DROP:
                redacted[key] = safe_item
        return redacted
    if isinstance(value, list):
        return [
            item
            for item in (_redact_metrics(item, cache, context) for item in value)
            if item is not _DROP
        ]
    return value


def _redact_observability_record(
    record: dict[str, Any], context: dict[str, str]
) -> dict[str, Any]:
    """Return a record whose sensitive metrics are redacted or removed."""
    safe_record = dict(record)
    metrics = safe_record.get("metrics")
    if isinstance(metrics, dict):
        safe_record["metrics"] = _redact_metrics(metrics, {}, context)
    return safe_record


def _record_observability(record: dict[str, Any], context: dict[str, str]) -> None:
    """Redact and persist one record through the public AgentSec CLI."""
    safe_record = _redact_observability_record(record, context)
    metrics = safe_record.get("metrics")
    if not isinstance(metrics, dict) or not metrics:
        _diagnostic("skipping record because no safe metrics remain after redaction")
        return

    command = _with_resolved_trace_context(_OBSERVABILITY_COMMAND, context)
    try:
        result = subprocess.run(
            command,
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
        details = _process_output_details(result.stderr, result.stdout)
        _diagnostic(
            "agent-sec-cli observability record failed "
            f"with exit code {result.returncode}: {details}"
        )


def main() -> None:
    """Read one Qoder hook event and record it without affecting execution."""
    if not _HOOK_ENABLED:
        return

    input_data: Any = None
    try:
        try:
            payload = _read_stdin_payload()
            if payload is None:
                return
            input_data = json.loads(payload)
        except (json.JSONDecodeError, EOFError, OSError, TypeError, ValueError):
            return
        if not isinstance(input_data, dict):
            return

        context = trace_context(input_data)
        record = _build_record(input_data, context)
        if record is not None:
            _record_observability(record, context)
    except Exception as exc:  # noqa: BLE001 - hook boundary must remain fail-open
        _unexpected_failure_diagnostic(exc, input_data)


if __name__ == "__main__":
    main()
