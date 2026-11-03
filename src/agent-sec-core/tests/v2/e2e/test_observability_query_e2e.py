"""V1 observability query scenarios over the installed V2 daemon protocol."""

import json
import os
import socket
import sqlite3
from datetime import datetime
from pathlib import Path

import pytest


@pytest.fixture
def query_daemon(tmp_path, monkeypatch, start_daemon):
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path / "data"))
    return start_daemon(admin_uids=[])


def _call(daemon, method, **params):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(5)
        client.connect(str(daemon.socket_path))
        client.sendall(
            json.dumps({"method": method, "params": params}).encode() + b"\n"
        )
        with client.makefile("rb") as response:
            result = json.loads(response.readline())
    assert result["requestId"]
    assert not {"ok", "data", "exit_code", "request_id"}.intersection(result)
    return result


def _seed(daemon, run_id="run-1", timestamp="2026-06-09T00:00:00+00:00"):
    """Use public ingestion and a same-owner security fixture without importing V1."""
    metadata = {"sessionId": "session-1", "runId": run_id}
    for hook, labels, metrics in [
        ("before_agent_run", metadata, {"user_input": "inspect coverage"}),
        (
            "before_tool_call",
            metadata | {"toolCallId": "tool-1"},
            {"parameters": {"command": "echo hi"}},
        ),
    ]:
        result = daemon.cli(
            "observability",
            "record",
            "--format",
            "json",
            "--stdin",
            input_text=json.dumps(
                {
                    "hook": hook,
                    "observedAt": timestamp,
                    "metadata": labels,
                    "metrics": metrics,
                }
            ),
        )
        assert result.returncode == 0, result.stderr
        assert result.stdout == ""

    with sqlite3.connect(
        Path(os.environ["AGENT_SEC_DATA_DIR"]) / "security-events.db"
    ) as store:
        store.execute(
            "INSERT INTO security_events "
            "(event_id,event_type,category,result,timestamp,timestamp_epoch,trace_id,"
            "pid,uid,session_id,run_id,tool_call_id,details) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                f"event-{run_id}",
                "scan",
                "code_scan",
                "succeeded",
                timestamp,
                datetime.fromisoformat(timestamp).timestamp(),
                "trace-1",
                os.getpid(),
                os.getuid(),
                "session-1",
                run_id,
                "tool-1",
                json.dumps(
                    {"request": {"code": "echo hi"}, "result": {"verdict": "pass"}}
                ),
            ),
        )


def test_sessions_runs_and_correlated_timeline(query_daemon):
    """Port V1's query-handler assertions to the V2 result envelope and owner fields."""
    _seed(query_daemon)
    sessions = _call(query_daemon, "obs.sessions.list")["result"]
    assert sessions["total"] == 1
    assert sessions["items"] == [
        {
            "uid": os.getuid(),
            "session_id": "session-1",
            "first_seen_epoch": 1780963200.0,
            "last_seen_epoch": 1780963200.0,
            "turn_count": 1,
            "observability_event_count": 2,
            "security_event_count": 1,
            "security_by_category_result": {"code_scan": {"succeeded": 1}},
        }
    ]
    runs = _call(query_daemon, "obs.runs.list", session_id="session-1")["result"]
    assert runs["items"][0]["run_id"] == "run-1"
    assert runs["items"][0]["user_input_preview"] == "inspect coverage"
    assert runs["items"][0]["security_event_count"] == 1

    timeline = _call(
        query_daemon, "obs.timeline.get", session_id="session-1", run_id="run-1"
    )["result"]
    assert (
        timeline["total"] == 2
    )  # Pagination counts observations, not correlated items.
    assert [item["kind"] for item in timeline["items"]].count("observability") == 2
    security = [item for item in timeline["items"] if item["kind"] == "security"]
    assert len(security) == 1
    item = security[0]
    assert item["event"]["event_id"] == "event-run-1"
    assert item["match"]["reason"] == "tool_call_id"
    assert item["observability_event_id"] == item["observability"]["id"]
    assert item["hook"] == item["observability"]["hook"] == "before_tool_call"
    assert item["session_id"] == "session-1"
    assert item["run_id"] == "run-1"
    assert (
        item["tool_call_id"]
        == item["observability"]["metadata"]["toolCallId"]
        == "tool-1"
    )
    assert all(item["uid"] == os.getuid() for item in timeline["items"])


def test_security_counts_respect_time_window(query_daemon):
    _seed(query_daemon, "run-old", "2026-06-08T00:00:00+00:00")
    _seed(query_daemon)
    nanos = {"start_ns": 1_780_963_199_000_000_000, "end_ns": 1_780_963_201_000_000_000}
    iso = {"since": "2026-06-08T23:59:59Z", "until": "2026-06-09T00:00:01Z"}
    for method, params in [
        ("obs.sessions.list", {}),
        ("obs.runs.list", {"session_id": "session-1"}),
    ]:
        result = _call(query_daemon, method, **params, **nanos)["result"]
        assert result == _call(query_daemon, method, **params, **iso)["result"]
        assert result["total"] == 1
        assert result["items"][0]["security_event_count"] == 1
        assert result["items"][0]["observability_event_count"] == 2
    assert result["items"][0]["run_id"] == "run-1"


def test_timeline_pages_observations_with_optional_security(query_daemon):
    _seed(query_daemon)
    params = {"session_id": "session-1", "run_id": "run-1", "limit": 1}
    first = _call(query_daemon, "obs.timeline.get", **params)["result"]
    assert first["total"] == 2
    assert first["next_offset"] == 1
    last = _call(
        query_daemon, "obs.timeline.get", **params, offset=first["next_offset"]
    )["result"]
    assert {item["kind"] for item in last["items"]} == {"observability", "security"}
    assert last["next_offset"] is None
    plain = _call(
        query_daemon, "obs.timeline.get", **params, offset=1, include_security=False
    )["result"]
    assert len(plain["items"]) == 1
    assert plain["items"][0]["kind"] == "observability"


@pytest.mark.parametrize(
    ("method", "params"),
    [
        ("obs.sessions.list", {"start_ns": 2_000_000_000, "end_ns": 1_000_000_000}),
        (
            "obs.runs.list",
            {
                "session_id": "session-1",
                "since": "1970-01-01T00:00:02Z",
                "end_ns": 1_000_000_000,
            },
        ),
        (
            "obs.timeline.get",
            {"session_id": "session-1", "run_id": "run-1", "end_ns": 10**100},
        ),
        ("obs.sessions.list", {"limit": 1001}),
        ("obs.sessions.list", {"offset": -1}),
        ("obs.runs.list", {}),
        ("obs.timeline.get", {"session_id": "session-1"}),
        ("obs.sessions.list", {"uid": 0}),
    ],
)
def test_query_validation_uses_v2_error_envelope(query_daemon, method, params):
    response = _call(query_daemon, method, **params)
    assert "result" not in response
    assert response["error"]["code"] == "invalid_argument"


@pytest.mark.parametrize(
    ("method", "params"),
    [
        ("obs.sessions.list", {"offset": 9_223_372_036_854_775_807}),
        ("obs.runs.list", {"session_id": "missing"}),
        ("obs.timeline.get", {"session_id": "missing", "run_id": "missing"}),
    ],
)
def test_empty_queries_succeed(query_daemon, method, params):
    response = _call(query_daemon, method, **params)["result"]
    assert response["items"] == []
    assert response["total"] == 0
    assert response["next_offset"] is None
