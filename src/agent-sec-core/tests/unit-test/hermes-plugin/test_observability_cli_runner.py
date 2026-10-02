"""Unit tests for Hermes observability CLI helper."""

from __future__ import annotations

import json
import subprocess
from unittest.mock import patch

from hermes_plugin_src.cli_runner import (
    CliResult,
    call_agent_sec_cli,
    record_hermes_observability,
    trace_context,
)


def _record() -> dict:
    return {
        "hook": "before_agent_run",
        "observedAt": "2026-05-18T00:00:00Z",
        "metadata": {
            "sessionId": "session-1",
            "runId": "00000000-0000-0000-0000-000000000000",
        },
        "metrics": {"user_input": "hello"},
    }


@patch("hermes_plugin_src.cli_runner.call_agent_sec_cli")
def test_record_hermes_observability_uses_openclaw_cli_shape(mock_cli):
    mock_cli.side_effect = [
        CliResult(
            stdout=json.dumps({"redacted_text": "hello"}),
            stderr="",
            exit_code=0,
        ),
        CliResult(stdout="", stderr="", exit_code=0),
    ]

    result = record_hermes_observability(_record(), timeout=5.0)

    assert result.exit_code == 0
    assert mock_cli.call_count == 2
    scan_args, scan_kwargs = mock_cli.call_args_list[0]
    assert scan_args[0] == [
        "scan-pii",
        "--stdin",
        "--format",
        "json",
        "--redact-output",
        "--source",
        "observability",
    ]
    assert scan_kwargs["stdin"] == "hello"
    args, kwargs = mock_cli.call_args_list[1]
    assert args[0] == ["observability", "record", "--format", "json", "--stdin"]
    assert kwargs["timeout"] == 5.0
    payload = json.loads(kwargs["stdin"])
    assert payload["hook"] == "before_agent_run"
    assert payload["metadata"]["sessionId"] == "session-1"


@patch("hermes_plugin_src.cli_runner.call_agent_sec_cli")
def test_record_hermes_observability_redacts_sensitive_payload(mock_cli):
    mock_cli.side_effect = [
        CliResult(
            stdout=json.dumps({"redacted_text": "a***@example.com"}),
            stderr="",
            exit_code=0,
        ),
        CliResult(stdout="", stderr="", exit_code=0),
    ]
    record = _record()
    record["metrics"]["user_input"] = "alice@example.com"

    record_hermes_observability(record, timeout=5.0)

    payload = json.loads(mock_cli.call_args_list[1].kwargs["stdin"])
    payload_text = json.dumps(payload, ensure_ascii=False)
    assert "alice@example.com" not in payload_text
    assert "a***@example.com" in payload_text


@patch("hermes_plugin_src.cli_runner.subprocess.run")
def test_call_agent_sec_cli_prepends_trace_context(mock_run):
    mock_run.return_value = subprocess.CompletedProcess(
        args=[],
        returncode=0,
        stdout="{}",
        stderr="",
    )

    result = call_agent_sec_cli(
        ["scan-code", "--code", "pwd"],
        timeout=5.0,
        trace_context={"session_id": "session-1", "tool_call_id": "tool-1"},
    )

    assert result.exit_code == 0
    argv = mock_run.call_args.args[0]
    assert argv[:4] == [
        "agent-sec-cli",
        "--trace-context",
        '{"session_id":"session-1","tool_call_id":"tool-1"}',
        "scan-code",
    ]


def test_trace_context_normalizes_camelcase_and_strips_whitespace():
    assert trace_context(
        {
            "traceId": " t1 ",
            "sessionId": "s1",
            "runId": "",
            "callId": "  ",
            "toolUseId": "u1",
            "agentName": "spoofed",
        }
    ) == {
        "agent_name": "hermes",
        "trace_id": "t1",
        "session_id": "s1",
        "tool_call_id": "u1",
    }


def test_trace_context_uses_first_non_empty_alias():
    assert trace_context(
        {
            "call_id": "",
            "callId": " c1 ",
            "tool_call_id": "",
            "toolCallId": "  ",
            "tool_use_id": " u1 ",
            "toolUseId": "u2",
            "agent_name": "spoofed",
        }
    ) == {"agent_name": "hermes", "call_id": "c1", "tool_call_id": "u1"}


def test_trace_context_returns_fixed_agent_name_when_all_fields_missing():
    assert trace_context({"foo": "bar"}) == {"agent_name": "hermes"}
