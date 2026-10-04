"""Session reports must tolerate malformed metric values.

Observability metric fields are typed ``Any`` in the public schema, so
``agent-sec-cli observability record`` accepts and stores scalars, lists, or
``null`` for counters such as ``request_payload_bytes`` and for
``tool_name``. Report aggregation must not crash on those values.
"""

import json
from unittest.mock import MagicMock

from agent_sec_cli.observability.session_report import (
    build_session_report,
    format_text,
)


def _reader_with(metrics_by_hook):
    session = MagicMock()
    session.session_id = "sess-1"
    session.first_seen_epoch = 1000.0
    session.last_seen_epoch = 1060.0
    session.turn_count = 1
    session.event_count = len(metrics_by_hook)

    run = MagicMock()
    run.run_id = "run-1"
    run.started_at_epoch = 1000.0
    run.ended_at_epoch = 1060.0
    run.user_input_preview = "hello"
    run.event_count = len(metrics_by_hook)

    events = []
    for hook, metrics in metrics_by_hook:
        event = MagicMock()
        event.hook = hook
        event.metrics_json = json.dumps(metrics)
        events.append(event)

    reader = MagicMock()
    reader.list_sessions.return_value = [session]
    reader.list_runs.return_value = [run]
    reader.list_events.return_value = events
    return reader


def test_non_numeric_payload_bytes_do_not_crash_report():
    reader = _reader_with(
        [
            (
                "after_llm_call",
                {
                    "latency_ms": 12,
                    "request_payload_bytes": "not-a-number",
                    "response_stream_bytes": None,
                },
            ),
            (
                "after_llm_call",
                {"request_payload_bytes": [1, 2], "response_stream_bytes": {"a": 1}},
            ),
        ]
    )

    report = build_session_report("sess-1", reader)

    assert report is not None
    assert report.llm_calls == 2
    assert report.request_bytes == 0
    assert report.response_bytes == 0
    assert "Payload:" not in format_text(report)


def test_numeric_metrics_still_aggregate():
    reader = _reader_with(
        [
            (
                "after_llm_call",
                {"request_payload_bytes": 1000, "response_stream_bytes": 2048},
            ),
            (
                "after_llm_call",
                {"request_payload_bytes": 24, "response_stream_bytes": 48},
            ),
        ]
    )

    report = build_session_report("sess-1", reader)

    assert report is not None
    assert report.llm_calls == 2
    assert report.request_bytes == 1024
    assert report.response_bytes == 2096


def test_unhashable_tool_name_does_not_crash_report():
    reader = _reader_with(
        [
            ("before_tool_call", {"tool_name": ["bash"]}),
            ("before_tool_call", {"tool_name": {"name": "bash"}}),
            ("before_tool_call", {"tool_name": 7}),
            ("before_tool_call", {"tool_name": "read_file"}),
        ]
    )

    report = build_session_report("sess-1", reader)

    assert report is not None
    assert report.tool_breakdown == {"unknown": 3, "read_file": 1}
