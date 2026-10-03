"""Tests for the session-report module."""

import json
from unittest.mock import MagicMock

from agent_sec_cli.observability.session_report import (
    SessionReport,
    build_session_report,
    format_text,
)


def _fake_session(sid="sess-1", first=1000.0, last=1060.0, turns=3, events=9):
    s = MagicMock()
    s.session_id = sid
    s.first_seen_epoch = first
    s.last_seen_epoch = last
    s.turn_count = turns
    s.event_count = events
    return s


def _fake_run(rid="run-1", start=1000.0, end=1020.0, preview="hello", events=3):
    r = MagicMock()
    r.run_id = rid
    r.started_at_epoch = start
    r.ended_at_epoch = end
    r.user_input_preview = preview
    r.event_count = events
    return r


def _fake_event(hook, metrics=None):
    e = MagicMock()
    e.hook = hook
    e.metrics_json = json.dumps(metrics or {})
    return e


def _fake_sec_event(category, result="succeeded"):
    e = MagicMock()
    e.category = category
    e.result = result
    return e


class TestBuildSessionReport:
    def test_unknown_session_returns_none(self):
        reader = MagicMock()
        reader.list_sessions.return_value = []
        rpt = build_session_report("nonexistent", reader)
        assert rpt is None

    def test_basic_session(self):
        reader = MagicMock()
        reader.list_sessions.return_value = [_fake_session()]
        reader.list_runs.return_value = [_fake_run()]
        reader.list_events.return_value = [
            _fake_event(
                "after_llm_call",
                {"request_payload_bytes": 1000, "response_stream_bytes": 200},
            ),
            _fake_event("before_tool_call", {"tool_name": "run_shell_command"}),
            _fake_event("before_tool_call", {"tool_name": "run_shell_command"}),
            _fake_event("before_tool_call", {"tool_name": "read_file"}),
        ]
        rpt = build_session_report("sess-1", reader)
        assert rpt.llm_calls == 1
        assert rpt.request_bytes == 1000
        assert rpt.response_bytes == 200
        assert rpt.tool_breakdown == {"run_shell_command": 2, "read_file": 1}
        assert rpt.turn_count == 3
        assert rpt.security_hint == "security-events DB not found"

    def test_security_verdicts(self):
        reader = MagicMock()
        reader.list_sessions.return_value = [_fake_session()]
        reader.list_runs.return_value = [_fake_run()]
        reader.list_events.return_value = []

        def _fake_candidate(category, result="succeeded"):
            c = MagicMock()
            c.event = _fake_sec_event(category, result)
            return c

        sec_reader = MagicMock()
        sec_reader.query_correlation_candidates.return_value = [
            _fake_candidate("code_scan", "succeeded"),
            _fake_candidate("code_scan", "succeeded"),
            _fake_candidate("code_scan", "failed"),
            _fake_candidate("prompt_scan", "succeeded"),
        ]
        rpt = build_session_report("sess-1", reader, sec_reader)
        assert rpt.security_verdicts["code_scan"] == {
            "succeeded": 2,
            "failed": 1,
        }
        assert rpt.security_verdicts["prompt_scan"] == {"succeeded": 1}
        assert rpt.security_hint == ""

    def test_security_empty_returns_hint(self):
        reader = MagicMock()
        reader.list_sessions.return_value = [_fake_session()]
        reader.list_runs.return_value = [_fake_run()]
        reader.list_events.return_value = []

        sec_reader = MagicMock()
        sec_reader.query_correlation_candidates.return_value = []
        rpt = build_session_report("sess-1", reader, sec_reader)
        assert rpt.security_verdicts == {}
        assert "session_id" in rpt.security_hint


class TestFormatText:
    def test_basic_format(self):
        rpt = SessionReport(
            session_id="abc123def456",
            first_seen="2026-06-03 14:30:00",
            last_seen="2026-06-03 15:02:00",
            duration_seconds=1920,
            turn_count=8,
            llm_calls=23,
            request_bytes=1200000,
            response_bytes=340000,
            tool_breakdown={
                "run_shell_command": 18,
                "read_file": 12,
                "skill": 2,
            },
            security_verdicts={
                "code_scan": {"succeeded": 15},
                "prompt_scan": {"succeeded": 23},
            },
        )
        text = format_text(rpt)
        assert "abc123def456" in text
        assert "8 turns" in text
        assert "23" in text
        assert "run_shell_command(18)" in text
        assert "code_scan" in text

    def test_hint_in_format(self):
        rpt = SessionReport(
            session_id="x",
            first_seen="",
            last_seen="",
            duration_seconds=0,
            turn_count=0,
            llm_calls=0,
            request_bytes=0,
            response_bytes=0,
            security_hint="hooks may not pass session_id",
        )
        text = format_text(rpt)
        assert "hooks may not pass session_id" in text

    def test_no_tools(self):
        rpt = SessionReport(
            session_id="x",
            first_seen="",
            last_seen="",
            duration_seconds=0,
            turn_count=0,
            llm_calls=0,
            request_bytes=0,
            response_bytes=0,
        )
        text = format_text(rpt)
        assert "(none)" in text


class TestToDict:
    def test_json_roundtrip(self):
        rpt = SessionReport(
            session_id="test",
            first_seen="2026-06-03 14:30:00",
            last_seen="2026-06-03 15:00:00",
            duration_seconds=1800,
            turn_count=5,
            llm_calls=10,
            request_bytes=50000,
            response_bytes=5000,
            tool_breakdown={"bash": 3},
            security_verdicts={"code_scan": {"succeeded": 5}},
        )
        d = rpt.to_dict()
        serialized = json.dumps(d)
        parsed = json.loads(serialized)
        assert parsed["session_id"] == "test"
        assert parsed["turn_count"] == 5
        assert parsed["security_hint"] is None
