"""Unit tests for Hermes observability record builder."""

# ruff: noqa: I001

from __future__ import annotations

from agent_sec_cli.observability import schema
from hermes_plugin_src.observability.record import ZERO_RUN_ID, build_record
from hermes_plugin_src.pii_text import text_sha256, value_to_text


def _assert_schema_valid(record: dict) -> None:
    validated = schema.validate_observability_record(record)
    wire_record = validated.to_record()
    assert wire_record["hook"] == record["hook"]
    assert wire_record["metadata"] == record["metadata"]
    assert wire_record["metrics"] == record["metrics"]


def test_pre_llm_call_builds_before_agent_record_from_current_input_only():
    record = build_record(
        "pre_llm_call",
        {
            "session_id": "session-1",
            "user_message": "hello",
            "conversation_history": [{"role": "user", "content": "hello"}],
            "model": "gpt-test",
            "platform": "hermes",
        },
        observed_at="2026-05-18T00:00:00Z",
    )

    assert record["hook"] == "before_agent_run"
    assert record["metadata"] == {"sessionId": "session-1", "runId": ZERO_RUN_ID}
    assert record["metrics"] == {
        "prompt": "hello",
        "user_input": "hello",
        "pii_scan_input_sha256": text_sha256("hello"),
        "model_id": "gpt-test",
        "model_provider": "hermes",
    }
    assert "history_messages_count" not in record["metrics"]
    _assert_schema_valid(record)


def test_pre_llm_call_hashes_history_without_changing_prompt_fields():
    record = build_record(
        "pre_llm_call",
        {
            "session_id": "session-1",
            "conversation_history": [
                {"role": "user", "content": "old request"},
                {"role": "assistant", "content": "old response"},
                {"role": "user", "content": [{"type": "text", "text": "latest input"}]},
            ],
            "model": "gpt-test",
            "platform": "hermes",
        },
        observed_at="2026-05-18T00:00:00Z",
    )

    assert record["metrics"]["prompt"] is None
    assert record["metrics"]["user_input"] is None
    assert record["metrics"]["pii_scan_input_sha256"] == text_sha256("latest input")
    _assert_schema_valid(record)


def test_pre_api_request_builds_before_llm_record_without_plugin_counters():
    record = build_record(
        "pre_api_request",
        {
            "session_id": "session-1",
            "task_id": "task-1",
            "api_call_count": 2,
            "model": "gpt-test",
            "provider": "openai",
            "api_mode": "chat",
            "base_url": "https://api.example.test",
            "message_count": 4,
            "approx_input_tokens": 100,
        },
        observed_at="2026-05-18T00:00:01Z",
    )

    assert record["hook"] == "before_llm_call"
    assert record["metadata"] == {
        "sessionId": "session-1",
        "runId": ZERO_RUN_ID,
        "callId": f"{ZERO_RUN_ID}:llm:2",
    }
    assert record["metrics"] == {
        "model_id": "gpt-test",
        "model_provider": "openai",
        "api": "chat",
        "transport": "https://api.example.test",
        "history_messages_count": 4,
    }
    assert "context_window_utilization" not in record["metrics"]
    _assert_schema_valid(record)


def test_post_api_request_omits_call_id_when_current_input_has_no_api_call_count():
    record = build_record(
        "post_api_request",
        {
            "session_id": "session-1",
            "api_duration": 123.4,
            "finish_reason": "stop",
            "assistant_tool_call_count": 0,
            "usage": {"prompt_tokens": 10},
        },
        observed_at="2026-05-18T00:00:02Z",
    )

    assert record["hook"] == "after_llm_call"
    assert record["metadata"] == {"sessionId": "session-1", "runId": ZERO_RUN_ID}
    assert record["metrics"] == {
        "latency_ms": 123.4,
        "stop_reason": "stop",
        "tool_calls_count": 0,
    }
    assert "response_stream_bytes" not in record["metrics"]
    _assert_schema_valid(record)


def test_pre_tool_call_requires_current_tool_call_id():
    record = build_record(
        "pre_tool_call",
        {
            "session_id": "session-1",
            "tool_call_id": "tool-1",
            "tool_name": "terminal",
            "args": {"command": "ls"},
        },
        observed_at="2026-05-18T00:00:03Z",
    )

    assert record["hook"] == "before_tool_call"
    assert record["metadata"] == {
        "sessionId": "session-1",
        "runId": ZERO_RUN_ID,
        "toolCallId": "tool-1",
    }
    assert record["metrics"] == {
        "tool_name": "terminal",
        "parameters": {"command": "ls"},
        "pii_scan_input_sha256": text_sha256(value_to_text({"command": "ls"})),
    }
    _assert_schema_valid(record)


def test_pre_tool_call_skips_when_tool_call_id_is_missing():
    record = build_record(
        "pre_tool_call",
        {
            "session_id": "session-1",
            "tool_name": "terminal",
            "args": {"command": "ls"},
        },
        observed_at="2026-05-18T00:00:03Z",
    )

    assert record is None


def test_post_tool_call_builds_after_tool_record_without_result_size_stats():
    record = build_record(
        "post_tool_call",
        {
            "session_id": "session-1",
            "tool_call_id": "tool-1",
            "tool_name": "terminal",
            "args": {"command": "ls"},
            "result": {"stdout": "ok", "exit_code": 0},
            "duration_ms": 5,
        },
        observed_at="2026-05-18T00:00:04Z",
    )

    assert record["hook"] == "after_tool_call"
    assert record["metadata"]["toolCallId"] == "tool-1"
    assert record["metrics"] == {
        "result": {"stdout": "ok", "exit_code": 0},
        "duration_ms": 5,
        "exit_code": 0,
        "error": None,
        "pii_scan_input_sha256": text_sha256(
            value_to_text({"stdout": "ok", "exit_code": 0})
        ),
    }
    assert "result_size_bytes" not in record["metrics"]
    assert "status" not in record["metrics"]
    _assert_schema_valid(record)


def test_post_llm_call_builds_after_agent_record_without_counts():
    record = build_record(
        "post_llm_call",
        {
            "session_id": "session-1",
            "assistant_response": "done",
            "model": "gpt-test",
            "platform": "hermes",
        },
        observed_at="2026-05-18T00:00:05Z",
    )

    assert record["hook"] == "after_agent_run"
    assert record["metadata"] == {"sessionId": "session-1", "runId": ZERO_RUN_ID}
    assert record["metrics"] == {
        "response": "done",
        "final_model_id": "gpt-test",
        "final_model_provider": "hermes",
    }
    assert "assistant_texts_count" not in record["metrics"]
    _assert_schema_valid(record)


def test_record_is_skipped_without_current_session_id():
    record = build_record(
        "post_api_request",
        {"task_id": "task-1", "api_duration": 123.4},
        observed_at="2026-05-18T00:00:06Z",
    )

    assert record is None
