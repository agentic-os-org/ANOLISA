"""Unit tests for cosh-extension/hooks/trace_context.py."""

import json
from pathlib import Path

from standalone_hook_test_loader import load_module_from_path

_HOOKS_DIR = Path(__file__).resolve().parents[2] / ".." / "cosh-extension" / "hooks"
trace_context_helper = load_module_from_path(
    "cosh_trace_context_helper",
    _HOOKS_DIR / "trace_context.py",
)
trace_context = trace_context_helper.trace_context
with_trace_context = trace_context_helper.with_trace_context


def test_trace_context_uses_fixed_cosh_hook_input_fields():
    assert trace_context(
        {
            "trace_id": "trace-1",
            "session_id": "session-1",
            "run_id": "run-1",
            "call_id": "call-1",
            "tool_use_id": "tool-1",
            "agent_name": "spoofed",
            "agentName": "spoofed",
        }
    ) == {
        "agent_name": "cosh",
        "trace_id": "trace-1",
        "session_id": "session-1",
        "run_id": "run-1",
        "call_id": "call-1",
        "tool_call_id": "tool-1",
    }


def test_trace_context_ignores_camel_case_and_empty_values():
    assert trace_context(
        {
            "trace_id": "",
            "traceId": "trace-1",
            "sessionId": "session-1",
            "runId": "run-1",
            "callId": "call-1",
            "toolUseId": "tool-1",
        }
    ) == {"agent_name": "cosh"}


def test_with_trace_context_serializes_fixed_fields():
    args = with_trace_context(
        ["agent-sec-cli", "scan-code"],
        {"session_id": "session-1", "run_id": "run-1"},
    )

    assert args == [
        "agent-sec-cli",
        "--trace-context",
        json.dumps(
            {"agent_name": "cosh", "session_id": "session-1", "run_id": "run-1"},
            ensure_ascii=False,
            separators=(",", ":"),
        ),
        "scan-code",
    ]


def test_with_trace_context_serializes_agent_name_without_other_fields():
    args = with_trace_context(["agent-sec-cli", "scan-code"], {"foo": "bar"})

    assert args == [
        "agent-sec-cli",
        "--trace-context",
        json.dumps(
            {"agent_name": "cosh"},
            ensure_ascii=False,
            separators=(",", ":"),
        ),
        "scan-code",
    ]
