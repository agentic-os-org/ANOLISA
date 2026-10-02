"""E2E tests for agent-sec-cli observability report command."""

import json

from .conftest import iso_now, run_cli


def _seed_session(session_id: str = "sess-e2e-report") -> None:
    """Insert observability events to create a session with LLM calls and tools."""
    events = [
        {
            "hook": "after_llm_call",
            "observedAt": iso_now(),
            "metadata": {
                "sessionId": session_id,
                "runId": "run-1",
                "callId": "call-1",
            },
            "metrics": {
                "request_payload_bytes": 500,
                "response_stream_bytes": 200,
            },
        },
        {
            "hook": "before_tool_call",
            "observedAt": iso_now(),
            "metadata": {
                "sessionId": session_id,
                "runId": "run-1",
                "callId": "call-1",
                "toolCallId": "tc-1",
            },
            "metrics": {"tool_name": "read_file"},
        },
        {
            "hook": "before_tool_call",
            "observedAt": iso_now(),
            "metadata": {
                "sessionId": session_id,
                "runId": "run-1",
                "callId": "call-1",
                "toolCallId": "tc-2",
            },
            "metrics": {"tool_name": "run_shell_command"},
        },
    ]
    for ev in events:
        result = run_cli(
            "observability",
            "record",
            "--format",
            "json",
            "--stdin",
            input_text=json.dumps(ev),
        )
        assert result.returncode == 0, f"seed failed: {result.stderr}"


def test_report_last_json() -> None:
    """--last --format json produces valid JSON with expected fields."""
    _seed_session()
    result = run_cli("observability", "report", "--last", "--format", "json")
    assert result.returncode == 0, result.stderr
    rpt = json.loads(result.stdout)
    assert rpt["session_id"] == "sess-e2e-report"
    assert rpt["llm_calls"] == 1
    assert rpt["request_bytes"] == 500
    assert rpt["response_bytes"] == 200
    assert rpt["tool_breakdown"]["read_file"] == 1
    assert rpt["tool_breakdown"]["run_shell_command"] == 1
    assert rpt["turn_count"] == 1


def test_report_last_text() -> None:
    """--last --format text produces human-readable output with specific values."""
    _seed_session()
    result = run_cli("observability", "report", "--last", "--format", "text")
    assert result.returncode == 0, result.stderr
    assert "500 bytes sent" in result.stdout
    assert "read_file(1)" in result.stdout


def test_report_session_id_json() -> None:
    """--session-id with known ID produces correct report."""
    _seed_session("sess-specific")
    result = run_cli(
        "observability",
        "report",
        "--session-id",
        "sess-specific",
        "--format",
        "json",
    )
    assert result.returncode == 0, result.stderr
    rpt = json.loads(result.stdout)
    assert rpt["session_id"] == "sess-specific"


def test_report_unknown_session_fails() -> None:
    """--session-id with unknown ID exits with code 1."""
    result = run_cli(
        "observability",
        "report",
        "--session-id",
        "nonexistent-session",
    )
    assert result.returncode == 1


def test_report_no_args_fails() -> None:
    """Missing both --session-id and --last exits with code 1."""
    result = run_cli("observability", "report")
    assert result.returncode == 1
    assert "specify" in result.stderr.lower() or "error" in result.stderr.lower()


def test_report_invalid_format_fails() -> None:
    """--format invalid exits with code 1."""
    _seed_session()
    result = run_cli("observability", "report", "--last", "--format", "xml")
    assert result.returncode == 1


def test_report_last_empty_db_fails() -> None:
    """--last on empty database exits with code 1."""
    result = run_cli("observability", "report", "--last")
    assert result.returncode == 1
