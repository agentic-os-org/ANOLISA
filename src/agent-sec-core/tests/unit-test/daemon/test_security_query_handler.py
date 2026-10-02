"""Tests for daemon security/observability SQLite query handlers."""

import asyncio
from pathlib import Path
from typing import Any

import agent_sec_cli.daemon.handlers.security_query as security_query
import pytest
from agent_sec_cli.daemon.protocol import DaemonRequest, DaemonResponse
from agent_sec_cli.daemon.registry import dispatch_request
from agent_sec_cli.daemon.runtime import DaemonRuntime
from agent_sec_cli.daemon.server import create_default_registry
from agent_sec_cli.observability.schema import validate_observability_record
from agent_sec_cli.observability.sqlite_writer import ObservabilitySqliteWriter
from agent_sec_cli.security_events.repositories import SecurityEventsSummary
from agent_sec_cli.security_events.schema import SecurityEvent
from agent_sec_cli.security_events.sqlite_writer import SqliteEventWriter


def _call_daemon(
    tmp_path: Path,
    method: str,
    params: dict[str, Any] | None = None,
) -> DaemonResponse:
    async def scenario() -> DaemonResponse:
        return await dispatch_request(
            DaemonRequest(
                method=method,
                params={} if params is None else params,
            ),
            create_default_registry(),
            DaemonRuntime(socket_path=tmp_path / "daemon.sock"),
        )

    return asyncio.run(scenario())


def _write_security_event(
    tmp_path: Path,
    *,
    event_id: str = "event-1",
    event_type: str = "code_scan",
    category: str = "code_scan",
    result: str = "succeeded",
    timestamp: str = "2026-06-09T00:00:00+00:00",
    session_id: str = "session-1",
    run_id: str = "run-1",
    tool_call_id: str | None = "tool-1",
    details: dict[str, Any] | None = None,
) -> None:
    writer = SqliteEventWriter(path=tmp_path / "security-events.db", max_age_days=None)
    try:
        writer.write(
            SecurityEvent(
                event_id=event_id,
                event_type=event_type,
                category=category,
                result=result,  # type: ignore[arg-type]
                timestamp=timestamp,
                trace_id="trace-1",
                session_id=session_id,
                run_id=run_id,
                tool_call_id=tool_call_id,
                details=(
                    details
                    if details is not None
                    else {"request": {"code": "echo hi"}, "result": {"valid": True}}
                ),
            )
        )
    finally:
        writer.close()


def _write_observability_event(
    tmp_path: Path,
    *,
    hook: str = "before_tool_call",
    observed_at: str = "2026-06-09T00:00:00Z",
    session_id: str = "session-1",
    run_id: str = "run-1",
    tool_call_id: str | None = "tool-1",
    metrics: dict[str, Any] | None = None,
) -> None:
    metadata: dict[str, Any] = {"sessionId": session_id, "runId": run_id}
    if tool_call_id is not None:
        metadata["toolCallId"] = tool_call_id
    writer = ObservabilitySqliteWriter(
        path=tmp_path / "observability.db",
        max_age_days=None,
    )
    try:
        writer.write_or_raise(
            validate_observability_record(
                {
                    "hook": hook,
                    "observedAt": observed_at,
                    "metadata": metadata,
                    "metrics": metrics or {"parameters": {"command": "echo hi"}},
                }
            )
        )
    finally:
        writer.close()


@pytest.fixture(autouse=True)
def _agent_sec_data_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))


def test_default_registry_exposes_dashboard_query_methods(tmp_path: Path) -> None:
    methods = set(create_default_registry().methods())

    assert "sec.status" not in methods
    assert {
        "sec.summary",
        "sec.events.list",
        "sec.events.get",
        "sec.events.count_by",
        "obs.sessions.list",
        "obs.runs.list",
        "obs.timeline.get",
    }.issubset(methods)


def test_security_event_queries_read_sqlite_data(tmp_path: Path) -> None:
    _write_security_event(tmp_path)
    _write_security_event(
        tmp_path,
        event_id="event-2",
        event_type="prompt_scan",
        category="prompt_scan",
        run_id="run-2",
        tool_call_id=None,
    )

    list_response = _call_daemon(
        tmp_path,
        "sec.events.list",
        {"session_id": "session-1", "limit": 10},
    )
    assert list_response.ok is True
    assert list_response.data["total"] == 2
    assert {item["event_id"] for item in list_response.data["items"]} == {
        "event-1",
        "event-2",
    }
    assert "details" not in list_response.data["items"][0]

    get_response = _call_daemon(
        tmp_path,
        "sec.events.get",
        {"event_id": "event-1"},
    )
    assert get_response.ok is True
    assert get_response.data["found"] is True
    assert get_response.data["event"]["details"]["request"]["code"] == "echo hi"

    count_response = _call_daemon(
        tmp_path,
        "sec.events.count_by",
        {"group_by": "run_id", "session_id": "session-1"},
    )
    assert count_response.ok is True
    assert count_response.data["items"] == [
        {"value": "run-1", "count": 1},
        {"value": "run-2", "count": 1},
    ]

    summary_response = _call_daemon(tmp_path, "sec.summary")
    assert summary_response.ok is True
    assert summary_response.data["total"] == 2
    assert summary_response.data["by_category"] == {
        "code_scan": 1,
        "prompt_scan": 1,
    }
    assert summary_response.data["affected_sessions"] == 1
    assert summary_response.data["affected_runs"] == 2


def test_skill_ledger_dashboard_fields_are_available_without_details(
    tmp_path: Path,
) -> None:
    details = {
        "request": {"command": "show", "skill_dir": "/opt/skills/demo"},
        "result": {
            "command": "show",
            "verdict": "deny",
            "skill_name": "demo",
            "latest_status": "deny",
        },
    }
    _write_security_event(
        tmp_path,
        event_id="skill-show",
        event_type="skill_ledger",
        category="skill_ledger",
        details=details,
    )

    list_response = _call_daemon(tmp_path, "sec.events.list")
    list_item = list_response.data["items"][0]
    assert list_item["verdict"] == "deny"
    assert list_item["command"] == "show"
    assert list_item["skill_name"] == "demo"
    assert "details" not in list_item

    get_response = _call_daemon(
        tmp_path,
        "sec.events.get",
        {"event_id": "skill-show"},
    )
    get_item = get_response.data["event"]
    assert get_item["verdict"] == "deny"
    assert get_item["command"] == "show"
    assert get_item["skill_name"] == "demo"
    assert get_item["details"] == details

    summary_response = _call_daemon(tmp_path, "sec.summary")
    latest_item = summary_response.data["latest_events"][0]
    assert latest_item["verdict"] == "deny"
    assert latest_item["command"] == "show"
    assert latest_item["skill_name"] == "demo"
    assert "details" not in latest_item


def test_skill_ledger_dashboard_skill_name_fallback_skips_batch_events() -> None:
    failed_event = SecurityEvent(
        event_type="skill_ledger",
        category="skill_ledger",
        result="failed",
        details={
            "request": {"command": "check", "skill_dir": "/opt/skills/fallback"},
            "result": {},
        },
    )
    failed_payload = security_query._event_payload(
        failed_event,
        include_details=False,
    )

    assert failed_payload["command"] == "check"
    assert failed_payload["skill_name"] == "fallback"
    assert "verdict" not in failed_payload

    batch_event = SecurityEvent(
        event_type="skill_ledger",
        category="skill_ledger",
        details={
            "request": {"command": "check", "skill_dir": "/opt/skills/not-a-skill"},
            "result": {
                "command": "check",
                "verdict": "tampered",
                "results": [{"skill_name": "a", "status": "tampered"}],
            },
        },
    )
    batch_payload = security_query._event_payload(
        batch_event,
        include_details=False,
    )

    assert batch_payload["command"] == "check"
    assert batch_payload["verdict"] == "tampered"
    assert "skill_name" not in batch_payload


def test_security_summary_handler_uses_repository_summary(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[dict[str, Any] | str] = []

    class SummaryReader:
        def summary(self, **kwargs: Any) -> SecurityEventsSummary:
            calls.append(kwargs)
            return SecurityEventsSummary(
                total=2,
                by_category={"code_scan": 2},
                by_event_type={"scan": 2},
                by_result={"succeeded": 2},
                by_session={"session-1": 1, None: 1},
                by_run={"run-1": 1, "": 1},
                latest_events=[
                    SecurityEvent(
                        event_id="latest-event",
                        event_type="scan",
                        category="code_scan",
                        details={"hidden": True},
                    )
                ],
            )

        def count(self, **_kwargs: Any) -> int:
            raise AssertionError("summary handler should not call count")

        def count_by(self, *_args: Any, **_kwargs: Any) -> dict[str, int]:
            raise AssertionError("summary handler should not call count_by")

        def query(self, **_kwargs: Any) -> list[SecurityEvent]:
            raise AssertionError("summary handler should not call query")

        def close(self) -> None:
            calls.append("close")

    monkeypatch.setattr(security_query, "SqliteEventReader", SummaryReader)

    result = security_query.security_summary_handler(
        DaemonRequest(
            method="sec.summary",
            params={"session_id": "session-1", "latest_limit": 7},
        ),
        DaemonRuntime(socket_path=tmp_path / "daemon.sock"),
    )

    assert calls == [
        {
            "event_type": None,
            "category": None,
            "result": None,
            "trace_id": None,
            "session_id": "session-1",
            "run_id": None,
            "call_id": None,
            "tool_call_id": None,
            "verdict": None,
            "since": None,
            "until": None,
            "latest_limit": 7,
        },
        "close",
    ]
    assert result.data["total"] == 2
    assert result.data["affected_sessions"] == 1
    assert result.data["affected_runs"] == 1
    latest_event = result.data["latest_events"][0]
    assert latest_event["event_id"] == "latest-event"
    assert latest_event["event_type"] == "scan"
    assert "details" not in latest_event


def test_observability_queries_read_sqlite_data(tmp_path: Path) -> None:
    _write_security_event(tmp_path)
    _write_observability_event(
        tmp_path,
        hook="before_agent_run",
        tool_call_id=None,
        metrics={"user_input": "inspect coverage"},
    )
    _write_observability_event(tmp_path)

    sessions_response = _call_daemon(tmp_path, "obs.sessions.list")
    assert sessions_response.ok is True
    assert sessions_response.data["items"] == [
        {
            "session_id": "session-1",
            "first_seen_epoch": 1780963200.0,
            "last_seen_epoch": 1780963200.0,
            "turn_count": 1,
            "observability_event_count": 2,
            "security_event_count": 1,
        }
    ]

    runs_response = _call_daemon(
        tmp_path,
        "obs.runs.list",
        {"session_id": "session-1"},
    )
    assert runs_response.ok is True
    assert runs_response.data["items"][0]["run_id"] == "run-1"
    assert runs_response.data["items"][0]["user_input_preview"] == "inspect coverage"
    assert runs_response.data["items"][0]["security_event_count"] == 1

    timeline_response = _call_daemon(
        tmp_path,
        "obs.timeline.get",
        {"session_id": "session-1", "run_id": "run-1"},
    )
    assert timeline_response.ok is True
    kinds = [item["kind"] for item in timeline_response.data["items"]]
    assert kinds.count("observability") == 2
    assert kinds.count("security") == 1
    security_item = next(
        item for item in timeline_response.data["items"] if item["kind"] == "security"
    )
    assert security_item["event"]["event_id"] == "event-1"
    assert security_item["match"]["reason"] == "tool_call_id"
    assert (
        security_item["observability_event_id"] == security_item["observability"]["id"]
    )
    assert security_item["hook"] == "before_tool_call"
    assert security_item["session_id"] == "session-1"
    assert security_item["run_id"] == "run-1"
    assert security_item["tool_call_id"] == "tool-1"
    assert security_item["observability"]["hook"] == "before_tool_call"
    assert security_item["observability"]["metadata"]["toolCallId"] == "tool-1"


def test_observability_security_counts_respect_time_window(tmp_path: Path) -> None:
    _write_observability_event(
        tmp_path,
        hook="before_agent_run",
        observed_at="2026-06-09T00:00:00Z",
        tool_call_id=None,
        metrics={"user_input": "current"},
    )
    _write_observability_event(
        tmp_path,
        hook="before_agent_run",
        observed_at="2026-06-08T00:00:00Z",
        run_id="run-old",
        tool_call_id=None,
        metrics={"user_input": "old"},
    )
    _write_security_event(
        tmp_path,
        event_id="current-security",
        timestamp="2026-06-09T00:00:00+00:00",
    )
    _write_security_event(
        tmp_path,
        event_id="old-security",
        timestamp="2026-06-08T00:00:00+00:00",
        run_id="run-old",
        tool_call_id=None,
    )

    start_ns = 1_780_963_199_000_000_000
    end_ns = 1_780_963_201_000_000_000
    sessions_response = _call_daemon(
        tmp_path,
        "obs.sessions.list",
        {"start_ns": start_ns, "end_ns": end_ns},
    )
    runs_response = _call_daemon(
        tmp_path,
        "obs.runs.list",
        {"session_id": "session-1", "start_ns": start_ns, "end_ns": end_ns},
    )

    assert sessions_response.ok is True
    assert sessions_response.data["total"] == 1
    assert sessions_response.data["items"][0]["turn_count"] == 1
    assert sessions_response.data["items"][0]["security_event_count"] == 1

    assert runs_response.ok is True
    assert runs_response.data["total"] == 1
    assert runs_response.data["items"][0]["run_id"] == "run-1"
    assert runs_response.data["items"][0]["security_event_count"] == 1


def test_security_query_validation_errors_are_bad_request(tmp_path: Path) -> None:
    invalid_group = _call_daemon(
        tmp_path,
        "sec.events.count_by",
        {"group_by": "invalid"},
    )
    assert invalid_group.ok is False
    assert invalid_group.error is not None
    assert invalid_group.error["code"] == "bad_request"

    invalid_limit = _call_daemon(
        tmp_path,
        "sec.events.list",
        {"limit": 5000},
    )
    assert invalid_limit.ok is False
    assert invalid_limit.error is not None
    assert invalid_limit.error["code"] == "bad_request"

    invalid_since = _call_daemon(
        tmp_path,
        "sec.events.list",
        {"since": "not-a-time"},
    )
    assert invalid_since.ok is False
    assert invalid_since.error is not None
    assert invalid_since.error["code"] == "bad_request"

    count_by_offset = _call_daemon(
        tmp_path,
        "sec.events.count_by",
        {"group_by": "category", "offset": 1},
    )
    assert count_by_offset.ok is False
    assert count_by_offset.error is not None
    assert count_by_offset.error["code"] == "bad_request"


@pytest.mark.parametrize(
    ("method", "params", "message"),
    [
        (
            "sec.events.list",
            {"result": "success"},
            "result must be one of: failed, succeeded",
        ),
        (
            "sec.events.list",
            {"result": ""},
            "result must be one of: failed, succeeded",
        ),
        (
            "sec.events.list",
            {
                "since": "2026-01-02T00:00:00Z",
                "until": "2026-01-01T00:00:00Z",
            },
            "time range start must not be after end",
        ),
        (
            "obs.sessions.list",
            {"start_ns": 2_000_000_000, "end_ns": 1_000_000_000},
            "time range start must not be after end",
        ),
        (
            "obs.runs.list",
            {
                "session_id": "session-1",
                "since": "1970-01-01T00:00:02Z",
                "end_ns": 1_000_000_000,
            },
            "time range start must not be after end",
        ),
        (
            "sec.events.list",
            {"start_ns": 10**100},
            "start_ns is outside the supported timestamp range",
        ),
        (
            "obs.timeline.get",
            {"session_id": "session-1", "run_id": "run-1", "end_ns": 10**100},
            "end_ns is outside the supported timestamp range",
        ),
        (
            "sec.events.list",
            {"offset": security_query._MAX_OFFSET + 1},
            f"offset must not exceed {security_query._MAX_OFFSET}",
        ),
    ],
)
def test_security_query_rejects_invalid_existing_parameter_values_before_reading(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    method: str,
    params: dict[str, Any],
    message: str,
) -> None:
    def fail_reader() -> None:
        raise AssertionError(
            "invalid parameters must be rejected before opening readers"
        )

    monkeypatch.setattr(security_query, "SqliteEventReader", fail_reader)
    monkeypatch.setattr(security_query, "ObservabilityReader", fail_reader)

    response = _call_daemon(tmp_path, method, params)

    assert response.ok is False
    assert response.error == {"code": "bad_request", "message": message}


def test_security_query_accepts_parameter_boundaries(tmp_path: Path) -> None:
    equal_time = "2026-01-01T00:00:00Z"
    _write_observability_event(tmp_path)

    result_response = _call_daemon(
        tmp_path,
        "sec.events.list",
        {"result": "failed", "since": equal_time, "until": equal_time},
    )
    offset_response = _call_daemon(
        tmp_path,
        "obs.sessions.list",
        {"offset": security_query._MAX_OFFSET},
    )

    assert result_response.ok is True
    assert result_response.data["items"] == []
    assert offset_response.ok is True
    assert offset_response.data["offset"] == security_query._MAX_OFFSET


def test_security_events_list_filters_by_verdict(tmp_path: Path) -> None:
    """Events list filtered by verdict returns only matching events."""
    _write_security_event(
        tmp_path,
        event_id="deny-1",
        details={"verdict": "deny"},
    )
    _write_security_event(
        tmp_path,
        event_id="deny-2",
        details={"verdict": "deny"},
    )
    _write_security_event(
        tmp_path,
        event_id="pass-1",
        details={"verdict": "pass"},
    )

    response = _call_daemon(
        tmp_path,
        "sec.events.list",
        {"verdict": "deny", "include_details": True},
    )
    assert response.ok is True
    assert response.data["total"] == 2
    assert len(response.data["items"]) == 2
    returned_ids = {item["event_id"] for item in response.data["items"]}
    assert returned_ids == {"deny-1", "deny-2"}
    assert all(item["verdict"] == "deny" for item in response.data["items"])


def test_security_events_list_normalizes_explicit_since_until(tmp_path: Path) -> None:
    _write_security_event(
        tmp_path,
        event_id="offset-window",
        timestamp="2026-05-20T04:00:00+00:00",
    )
    _write_security_event(
        tmp_path,
        event_id="outside-window",
        timestamp="2026-05-20T06:00:00+00:00",
    )

    response = _call_daemon(
        tmp_path,
        "sec.events.list",
        {
            "since": "2026-05-20T11:59:00+08:00",
            "until": "2026-05-20T12:01:00+08:00",
        },
    )

    assert response.ok is True
    assert [item["event_id"] for item in response.data["items"]] == ["offset-window"]


def test_security_events_count_by_verdict_group(tmp_path: Path) -> None:
    """Count-by with group_by=verdict returns correct grouped counts."""
    _write_security_event(tmp_path, event_id="d1", details={"verdict": "deny"})
    _write_security_event(tmp_path, event_id="d2", details={"verdict": "deny"})
    _write_security_event(tmp_path, event_id="p1", details={"verdict": "pass"})
    _write_security_event(tmp_path, event_id="n1", details={})

    response = _call_daemon(
        tmp_path,
        "sec.events.count_by",
        {"group_by": "verdict"},
    )
    assert response.ok is True
    items = {item["value"]: item["count"] for item in response.data["items"]}
    assert items["deny"] == 2
    assert items["pass"] == 1
    # Event with no verdict should not appear in groups
    assert "" not in items


def test_security_summary_respects_verdict_filter(tmp_path: Path) -> None:
    """Summary with verdict filter only counts matching events."""
    _write_security_event(
        tmp_path,
        event_id="deny-1",
        category="code_scan",
        details={"verdict": "deny"},
    )
    _write_security_event(
        tmp_path,
        event_id="deny-2",
        category="prompt_scan",
        event_type="prompt_scan",
        details={"verdict": "deny"},
    )
    _write_security_event(
        tmp_path,
        event_id="pass-1",
        category="code_scan",
        details={"verdict": "pass"},
    )

    response = _call_daemon(
        tmp_path,
        "sec.summary",
        {"verdict": "deny"},
    )
    assert response.ok is True
    assert response.data["total"] == 2
    assert response.data["by_category"] == {"code_scan": 1, "prompt_scan": 1}
