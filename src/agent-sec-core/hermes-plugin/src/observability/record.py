from __future__ import annotations

from typing import Any

from ..pii_text import extract_user_text, text_sha256, value_to_text
from .helpers import non_empty_string, now_iso

ZERO_RUN_ID = "00000000-0000-0000-0000-000000000000"


def build_record(
    hook_name: str,
    data: dict[str, Any],
    observed_at: str | None = None,
) -> dict[str, Any] | None:
    observed = observed_at or now_iso()
    builders = {
        "pre_llm_call": _build_pre_llm_call,
        "pre_api_request": _build_pre_api_request,
        "post_api_request": _build_post_api_request,
        "pre_tool_call": _build_pre_tool_call,
        "post_tool_call": _build_post_tool_call,
        "post_llm_call": _build_post_llm_call,
    }
    builder = builders.get(hook_name)
    if builder is None:
        return None
    return builder(data, observed)


def _base_record(
    hook: str,
    observed_at: str,
    metadata: dict[str, Any] | None,
    metrics: dict[str, Any],
) -> dict[str, Any] | None:
    if metadata is None:
        return None
    if not metrics:
        return None
    return {
        "hook": hook,
        "observedAt": observed_at,
        "metadata": metadata,
        "metrics": metrics,
    }


def _metadata(
    data: dict[str, Any],
    *,
    require_tool_call_id: bool = False,
) -> dict[str, Any] | None:
    session_id = non_empty_string(data.get("session_id"))
    if session_id is None:
        return None

    metadata: dict[str, Any] = {
        "sessionId": session_id,
        "runId": ZERO_RUN_ID,
    }

    call_id = _call_id(data)
    if call_id is not None:
        metadata["callId"] = call_id

    if require_tool_call_id:
        tool_call_id = non_empty_string(data.get("tool_call_id"))
        if tool_call_id is None:
            return None
        metadata["toolCallId"] = tool_call_id

    return metadata


def _call_id(data: dict[str, Any]) -> str | None:
    call_id = non_empty_string(data.get("call_id"))
    if call_id is not None:
        return call_id
    api_call_count = non_empty_string(data.get("api_call_count"))
    if api_call_count is None:
        return None
    return f"{ZERO_RUN_ID}:llm:{api_call_count}"


def _build_pre_llm_call(
    data: dict[str, Any],
    observed_at: str,
) -> dict[str, Any] | None:
    user_message = data.get("user_message")
    pii_hash = _pii_scan_input_sha256(
        extract_user_text(data.get("conversation_history"), data)
    )
    metrics = {
        "prompt": user_message,
        "user_input": user_message,
        "model_id": data.get("model"),
        "model_provider": data.get("platform"),
    }
    if pii_hash is not None:
        metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(
        "before_agent_run",
        observed_at,
        _metadata(data),
        metrics,
    )


def _build_pre_api_request(
    data: dict[str, Any],
    observed_at: str,
) -> dict[str, Any] | None:
    return _base_record(
        "before_llm_call",
        observed_at,
        _metadata(data),
        {
            "model_id": data.get("model"),
            "model_provider": data.get("provider"),
            "api": data.get("api_mode"),
            "transport": data.get("base_url"),
            "history_messages_count": data.get("message_count"),
        },
    )


def _build_post_api_request(
    data: dict[str, Any],
    observed_at: str,
) -> dict[str, Any] | None:
    return _base_record(
        "after_llm_call",
        observed_at,
        _metadata(data),
        {
            "latency_ms": data.get("api_duration"),
            "stop_reason": data.get("finish_reason"),
            "tool_calls_count": data.get("assistant_tool_call_count"),
        },
    )


def _build_pre_tool_call(
    data: dict[str, Any],
    observed_at: str,
) -> dict[str, Any] | None:
    args = data.get("args")
    metrics = {
        "tool_name": data.get("tool_name"),
        "parameters": args,
    }
    pii_hash = _pii_scan_input_sha256(args)
    if pii_hash is not None:
        metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(
        "before_tool_call",
        observed_at,
        _metadata(data, require_tool_call_id=True),
        metrics,
    )


def _extract_exit_code(result: Any) -> Any:
    if isinstance(result, dict):
        if "exit_code" in result:
            return result["exit_code"]
        if "exitCode" in result:
            return result["exitCode"]
    return None


def _pii_scan_input_sha256(value: Any) -> str | None:
    text = value_to_text(value)
    if not text.strip():
        return None
    return text_sha256(text)


def _build_post_tool_call(
    data: dict[str, Any],
    observed_at: str,
) -> dict[str, Any] | None:
    result = data.get("result")
    metrics = {
        "result": result,
        "duration_ms": data.get("duration_ms"),
        "exit_code": _extract_exit_code(result),
        "error": data.get("error"),
    }
    pii_hash = _pii_scan_input_sha256(result)
    if pii_hash is not None:
        metrics["pii_scan_input_sha256"] = pii_hash
    return _base_record(
        "after_tool_call",
        observed_at,
        _metadata(data, require_tool_call_id=True),
        metrics,
    )


def _build_post_llm_call(
    data: dict[str, Any],
    observed_at: str,
) -> dict[str, Any] | None:
    return _base_record(
        "after_agent_run",
        observed_at,
        _metadata(data),
        {
            "response": data.get("assistant_response"),
            "final_model_id": data.get("model"),
            "final_model_provider": data.get("platform"),
        },
    )
