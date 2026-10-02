"""Unit tests for observability record payload validation."""

import pytest
from agent_sec_cli.correlation_context import (
    MAX_CORRELATION_ID_LENGTH,
    TRUNCATED_CORRELATION_ID_SUFFIX,
)
from agent_sec_cli.observability.metrics import HOOK_METRIC_ALLOWLIST
from agent_sec_cli.observability.schema import (
    validate_observability_record,
)
from pydantic import ValidationError

MINIMAL_METRICS_BY_HOOK = {
    "before_agent_run": {"prompt": "Summarize ./README.md"},
    "before_llm_call": {"model_id": "gpt-example"},
    "after_llm_call": {"outcome": "success"},
    "before_tool_call": {"tool_name": "read_file"},
    "after_tool_call": {"duration_ms": 25},
    "after_agent_run": {"response": "Done."},
}

CALL_ID_HOOKS = {"before_llm_call", "after_llm_call"}
TOOL_CALL_HOOKS = {"before_tool_call", "after_tool_call"}


def _payload(**overrides):
    hook = overrides.get("hook", "before_agent_run")
    payload = {
        "hook": hook,
        "observedAt": "2026-05-11T12:00:00Z",
        "metadata": _metadata_for_hook(hook),
        "metrics": {"prompt": "Summarize ./README.md"},
    }
    payload.update(overrides)
    return payload


def _metadata_for_hook(hook):
    metadata = {
        "sessionId": "session-123",
        "runId": "run-123",
    }
    if hook in CALL_ID_HOOKS:
        metadata["callId"] = "model-call-1"
    if hook in TOOL_CALL_HOOKS:
        metadata["toolCallId"] = "tool-call-1"
    return metadata


def test_minimal_metric_examples_cover_each_hook():
    assert set(MINIMAL_METRICS_BY_HOOK) == set(HOOK_METRIC_ALLOWLIST)


@pytest.mark.parametrize(("hook", "metrics"), MINIMAL_METRICS_BY_HOOK.items())
def test_each_hook_accepts_minimal_allowed_metric(hook, metrics):
    record = validate_observability_record(_payload(hook=hook, metrics=metrics))

    assert record.hook == hook
    assert record.to_record()["metrics"] == metrics
    assert record.metadata.session_id == "session-123"
    assert record.metadata.run_id == "run-123"
    assert record.observed_at.tzinfo is not None


def test_camel_case_payload_dumps_back_to_wire_aliases():
    record = validate_observability_record(_payload())

    dumped = record.to_record()

    assert "schemaVersion" not in dumped
    assert "observedAt" in dumped
    assert dumped["metadata"]["sessionId"] == "session-123"
    assert dumped["metadata"]["runId"] == "run-123"


def test_observability_metadata_truncates_long_correlation_ids_with_suffix():
    long_value = "x" * (MAX_CORRELATION_ID_LENGTH + 10)
    expected = (
        "x" * (MAX_CORRELATION_ID_LENGTH - len(TRUNCATED_CORRELATION_ID_SUFFIX))
        + TRUNCATED_CORRELATION_ID_SUFFIX
    )

    record = validate_observability_record(
        _payload(
            hook="before_tool_call",
            metadata={
                "sessionId": long_value,
                "runId": long_value,
                "callId": long_value,
                "toolCallId": long_value,
            },
            metrics={"tool_name": "read_file"},
        )
    )

    dumped_metadata = record.to_record()["metadata"]
    assert dumped_metadata == {
        "sessionId": expected,
        "runId": expected,
        "callId": expected,
        "toolCallId": expected,
    }
    assert len(record.metadata.session_id) == MAX_CORRELATION_ID_LENGTH
    assert len(record.metadata.run_id) == MAX_CORRELATION_ID_LENGTH
    assert len(record.metadata.call_id or "") == MAX_CORRELATION_ID_LENGTH
    assert len(record.metadata.tool_call_id) == MAX_CORRELATION_ID_LENGTH


def test_all_allowed_metrics_are_not_required():
    record = validate_observability_record(
        _payload(
            hook="before_agent_run",
            metrics={"system_prompt": "You are a concise assistant."},
        )
    )

    assert record.to_record()["metrics"] == {
        "system_prompt": "You are a concise assistant."
    }


def test_before_agent_run_accepts_run_start_metrics():
    metrics = {
        "prompt": "Summarize ./README.md",
        "system_prompt": "You are a concise assistant.",
    }

    record = validate_observability_record(
        _payload(hook="before_agent_run", metrics=metrics)
    )

    assert record.to_record()["metrics"] == metrics


def test_before_agent_run_accepts_input_context_metrics():
    metrics = {
        "prompt": [{"role": "user", "content": "Summarize ./README.md"}],
        "system_prompt": "You are a concise assistant.",
        "user_input": "Summarize ./README.md",
        "pii_scan_input_sha256": "b" * 64,
        "history_messages_count": 3,
        "images_count": 1,
        "context_window_utilization": 0.25,
        "model_id": "gpt-example",
        "model_provider": "openai",
    }

    record = validate_observability_record(
        _payload(
            hook="before_agent_run",
            metrics=metrics,
        )
    )

    dumped = record.to_record()
    assert dumped["metrics"] == metrics
    assert "callId" not in dumped["metadata"]
    assert "call_id" not in dumped["metrics"]


def test_before_llm_call_accepts_complete_model_call_metrics():
    metrics = {
        "prompt": [{"role": "user", "content": "Summarize ./README.md"}],
        "system_prompt": "You are a concise assistant.",
        "user_input": "Summarize ./README.md",
        "history_messages_count": 3,
        "images_count": 1,
        "context_window_utilization": 0.25,
        "model_id": "gpt-example",
        "model_provider": "openai",
        "api": "chat.completions",
        "transport": "http",
    }

    record = validate_observability_record(
        _payload(
            hook="before_llm_call",
            metadata={
                "sessionId": "session-123",
                "runId": "run-123",
                "callId": "model-call-1",
            },
            metrics=metrics,
        )
    )

    dumped = record.to_record()
    assert dumped["metrics"] == metrics
    assert dumped["metadata"]["callId"] == "model-call-1"


def test_after_llm_call_accepts_model_call_ended_metrics():
    metrics = {
        "latency_ms": 250,
        "outcome": "failure",
        "error_category": "network",
        "failure_kind": "timeout",
        "request_payload_bytes": 1024,
        "response_stream_bytes": 128,
        "time_to_first_byte_ms": 75,
        "upstream_request_id_hash": "sha256:abc123",
    }

    record = validate_observability_record(
        _payload(hook="after_llm_call", metrics=metrics)
    )

    dumped = record.to_record()
    assert dumped["metadata"]["callId"] == "model-call-1"
    assert dumped["metrics"] == metrics


def test_after_agent_run_accepts_llm_output_response():
    record = validate_observability_record(
        _payload(
            hook="after_agent_run",
            metadata={"sessionId": "session-123", "runId": "run-123"},
            metrics={"response": "Done."},
        )
    )

    dumped = record.to_record()
    assert dumped["metadata"] == {"sessionId": "session-123", "runId": "run-123"}
    assert dumped["metrics"] == {"response": "Done."}


def test_after_agent_run_accepts_llm_output_tool_use_summary():
    metrics = {
        "output_kind": "tool_use",
        "stop_reason": "toolUse",
        "assistant_texts_count": 0,
        "tool_calls_count": 1,
        "tool_calls": [
            {
                "toolName": "exec",
                "parameters": {
                    "command": 'find /home/xingdong -name "testfolder2" -maxdepth 3 2>/dev/null'
                },
            }
        ],
    }

    record = validate_observability_record(
        _payload(
            hook="after_agent_run",
            metadata={"sessionId": "session-123", "runId": "run-123"},
            metrics=metrics,
        )
    )

    dumped = record.to_record()
    assert dumped["metadata"] == {"sessionId": "session-123", "runId": "run-123"}
    assert dumped["metrics"] == metrics


def test_after_llm_call_accepts_llm_output_response_without_call_id():
    record = validate_observability_record(
        _payload(
            hook="after_llm_call",
            metadata={"sessionId": "session-123", "runId": "run-123"},
            metrics={"response": "Done."},
        )
    )

    dumped = record.to_record()
    assert dumped["metadata"] == {"sessionId": "session-123", "runId": "run-123"}
    assert dumped["metrics"] == {"response": "Done."}


def test_after_llm_call_accepts_llm_output_tool_use_summary_without_call_id():
    metrics = {
        "output_kind": "tool_use",
        "stop_reason": "toolUse",
        "assistant_texts_count": 0,
        "tool_calls_count": 1,
        "tool_calls": [
            {
                "toolName": "exec",
                "parameters": {
                    "command": 'find /home/xingdong -name "testfolder2" -maxdepth 3 2>/dev/null'
                },
            }
        ],
    }

    record = validate_observability_record(
        _payload(
            hook="after_llm_call",
            metadata={"sessionId": "session-123", "runId": "run-123"},
            metrics=metrics,
        )
    )

    dumped = record.to_record()
    assert dumped["metadata"] == {"sessionId": "session-123", "runId": "run-123"}
    assert dumped["metrics"] == metrics


def test_after_llm_call_drops_unsupported_response_detail_metrics():
    with pytest.raises(ValidationError, match="at least one allowed metric"):
        validate_observability_record(
            _payload(
                hook="after_llm_call",
                metrics={
                    "finish_reason": "stop",
                },
            )
        )


def test_tool_call_records_dump_tool_call_id():
    record = validate_observability_record(
        _payload(hook="before_tool_call", metrics={"tool_name": "read_file"})
    )

    assert record.to_record()["metadata"]["toolCallId"] == "tool-call-1"


def test_before_tool_call_accepts_pii_input_hash_metric():
    metrics = {
        "tool_name": "write_file",
        "parameters": {"path": "note.txt"},
        "pii_scan_input_sha256": "c" * 64,
    }

    record = validate_observability_record(
        _payload(hook="before_tool_call", metrics=metrics)
    )

    assert record.to_record()["metadata"]["toolCallId"] == "tool-call-1"
    assert record.to_record()["metrics"] == metrics


def test_after_tool_call_accepts_query_friendly_result_metrics():
    metrics = {
        "result": {"ok": True},
        "error": "command failed",
        "pii_scan_input_sha256": "a" * 64,
        "duration_ms": 123,
        "status": "error",
        "exit_code": 1,
        "result_size_bytes": 2048,
    }

    record = validate_observability_record(
        _payload(hook="after_tool_call", metrics=metrics)
    )

    assert record.to_record()["metadata"]["toolCallId"] == "tool-call-1"
    assert record.to_record()["metrics"] == metrics


def test_after_agent_run_accepts_final_summary_metrics():
    metrics = {
        "response": "Done.",
        "success": True,
        "error": None,
        "duration_ms": 500,
        "total_api_calls": 2,
        "total_tool_calls": 1,
        "final_model_id": "gpt-example",
        "final_model_provider": "openai",
    }

    record = validate_observability_record(
        _payload(hook="after_agent_run", metrics=metrics)
    )

    assert record.to_record()["metrics"] == metrics


def test_before_agent_run_accepts_assembled_input_metrics():
    metrics = {
        "prompt": [{"role": "user", "content": "Summarize ./README.md"}],
        "system_prompt": "You are a concise assistant.",
        "user_input": "Summarize ./README.md",
    }

    record = validate_observability_record(
        _payload(hook="before_agent_run", metrics=metrics)
    )

    assert record.to_record()["metrics"] == metrics


def test_before_agent_run_accepts_input_records_without_call_id():
    record = validate_observability_record(
        _payload(
            hook="before_agent_run",
            metadata={"sessionId": "session-123", "runId": "run-123"},
            metrics={"prompt": "assembled prompt"},
        )
    )

    dumped = record.to_record()
    assert dumped["metadata"] == {"sessionId": "session-123", "runId": "run-123"}
    assert dumped["metrics"] == {"prompt": "assembled prompt"}


def test_after_llm_call_accepts_missing_call_id():
    record = validate_observability_record(
        _payload(
            hook="after_llm_call",
            metadata={"sessionId": "session-123", "runId": "run-123"},
            metrics={"outcome": "success"},
        )
    )

    dumped = record.to_record()
    assert dumped["metadata"] == {"sessionId": "session-123", "runId": "run-123"}
    assert dumped["metrics"] == {"outcome": "success"}


def test_tool_call_metadata_requires_tool_call_id():
    with pytest.raises(ValidationError):
        validate_observability_record(
            _payload(
                hook="after_tool_call",
                metadata={"sessionId": "session-123", "runId": "run-123"},
                metrics={"duration_ms": 25},
            )
        )


@pytest.mark.parametrize("field_name", ("sessionId", "runId"))
def test_common_metadata_requires_session_id_and_run_id(field_name):
    metadata = _metadata_for_hook("before_agent_run")
    metadata.pop(field_name)

    with pytest.raises(ValidationError):
        validate_observability_record(_payload(metadata=metadata))


@pytest.mark.parametrize("field_name", ("sessionId", "runId"))
def test_empty_session_id_or_run_id_is_allowed(field_name):
    metadata = _metadata_for_hook("before_agent_run")
    metadata[field_name] = ""

    record = validate_observability_record(_payload(metadata=metadata))

    dumped = record.to_record()
    assert dumped["metadata"][field_name] == ""


@pytest.mark.parametrize(
    "payload",
    (
        {"metadata": ["not", "an", "object"]},
        {"metadata": {"sessionId": 123, "runId": "run-123"}},
        {"metrics": ["not", "an", "object"]},
        {
            "hook": "after_llm_call",
            "metadata": {
                "sessionId": "session-123",
                "runId": "run-123",
                "callId": 123,
            },
            "metrics": {"outcome": "success"},
        },
        {
            "hook": "after_tool_call",
            "metadata": {
                "sessionId": "session-123",
                "runId": "run-123",
                "toolCallId": 123,
            },
            "metrics": {"duration_ms": 25},
        },
    ),
)
def test_invalid_payload_values_fail_validation(payload):
    with pytest.raises(ValidationError):
        validate_observability_record(_payload(**payload))


def test_after_tool_call_uses_duration_ms():
    record = validate_observability_record(
        _payload(hook="after_tool_call", metrics={"duration_ms": 123})
    )

    assert record.to_record()["metrics"] == {"duration_ms": 123}


def test_unknown_hook_fails():
    with pytest.raises(ValidationError, match="unknown observability hook"):
        validate_observability_record(_payload(hook="during_agent_run"))


def test_before_context_assembly_is_not_supported():
    with pytest.raises(ValidationError, match="unknown observability hook"):
        validate_observability_record(
            _payload(
                hook="before_context_assembly",
                metrics={"system_prompt": "You are a concise assistant."},
            )
        )


def test_after_llm_response_is_not_supported():
    with pytest.raises(ValidationError, match="unknown observability hook"):
        validate_observability_record(
            _payload(
                hook="after_llm_response",
                metrics={"outcome": "success"},
            )
        )


@pytest.mark.parametrize(
    ("hook", "metric"),
    (
        ("after_llm_call", "call_id"),
        ("after_llm_call", "call_index"),
        ("after_llm_call", "finish_reason"),
        ("after_llm_call", "total_api_calls"),
        ("after_tool_call", "duration"),
        ("after_tool_call", "result_row_count"),
        ("before_agent_run", "prompt_length_chars"),
        ("before_agent_run", "prompt_length_tokens"),
        ("before_agent_run", "encoding_anomalies"),
        ("before_agent_run", "contains_url"),
        ("before_agent_run", "contains_file_path"),
        ("before_agent_run", "contains_code_snippet"),
        ("before_agent_run", "input_tokens_estimated"),
        ("before_agent_run", "tools_available_count"),
        ("before_agent_run", "tools_available"),
        ("before_llm_call", "call_id"),
        ("before_llm_call", "call_index"),
        ("before_llm_call", "estimated_input_tokens"),
        ("before_llm_call", "history_tokens"),
        ("before_llm_call", "system_prompt_hash"),
        ("before_llm_call", "system_prompt_tokens"),
        ("before_llm_call", "user_input_tokens"),
    ),
)
def test_deprecated_metrics_are_dropped_and_rejected_when_empty(hook, metric):
    with pytest.raises(ValidationError, match="at least one allowed metric"):
        validate_observability_record(_payload(hook=hook, metrics={metric: True}))


def test_unknown_metric_is_dropped_when_supported_metrics_remain():
    record = validate_observability_record(
        _payload(metrics={"prompt": "ok", "unlisted_metric": 1})
    )

    assert record.to_record()["metrics"] == {"prompt": "ok"}


def test_only_unknown_metrics_fails():
    with pytest.raises(ValidationError, match="at least one allowed metric"):
        validate_observability_record(_payload(metrics={"unlisted_metric": 1}))


def test_extra_top_level_and_metadata_fields_are_dropped():
    record = validate_observability_record(
        _payload(
            producerVersion="2.0.0",
            metadata={
                "sessionId": "session-123",
                "runId": "run-123",
                "futureCorrelationId": "future-123",
            },
        )
    )

    dumped = record.to_record()
    assert "producerVersion" not in dumped
    assert "futureCorrelationId" not in dumped["metadata"]


def test_empty_metrics_fails():
    with pytest.raises(ValidationError, match="at least one allowed metric"):
        validate_observability_record(_payload(metrics={}))


def test_invalid_timestamp_fails():
    with pytest.raises(ValidationError):
        validate_observability_record(_payload(observedAt="not-a-timestamp"))


def test_naive_timestamp_fails():
    with pytest.raises(ValidationError, match="timezone-aware"):
        validate_observability_record(_payload(observedAt="2026-05-11T12:00:00"))
