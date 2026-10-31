from __future__ import annotations

import json
from pathlib import Path

import pytest
from ce_runner.session_trace_converter import convert_session_to_trace


def dispatches(
    tmp_path: Path, task: dict, raw_name: str, command: str | None = None
) -> list[dict]:
    source = tmp_path / "session.jsonl"
    output = tmp_path / "trace.jsonl"
    arguments = {"value": 12} if command is None else {"command": command}
    records = [
        {
            "type": "message",
            "timestamp": "2026-01-01T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "toolCall",
                        "id": "call",
                        "name": raw_name,
                        "arguments": arguments,
                    }
                ],
            },
        },
        {
            "type": "message",
            "timestamp": "2026-01-01T00:00:01Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "call",
                "content": [{"type": "text", "text": "result"}],
            },
        },
    ]
    source.write_text(
        "\n".join(json.dumps(record) for record in records), encoding="utf-8"
    )
    metadata = convert_session_to_trace(
        str(source), task, str(output), preloaded_audit_data={}
    )
    found = [
        json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()
    ]
    rows = [row for row in found if row["type"] == "tool_dispatch"]
    assert metadata["tool_dispatches"] == len(rows)
    return rows


@pytest.mark.parametrize("raw_name", ["lookup", "mcp__lookup"])
@pytest.mark.parametrize("url", ["", None, "http://fixture.invalid/first"])
def test_direct_dispatch_retains_first_endpoint(tmp_path, raw_name, url):
    task = {
        "tools": [{"name": "lookup"}, {"name": "lookup"}],
        "tool_endpoints": [
            {"tool_name": "lookup", "url": url},
            {"tool_name": "lookup", "url": "http://fixture.invalid/later"},
        ],
    }
    (row,) = dispatches(tmp_path, task, raw_name)
    assert row["tool_name"] == "lookup"
    assert row["endpoint_url"] == url
    assert row["request_body"] == {"value": 12}
    assert row["response_status"] == 200 and row["response_body"] == "result"


@pytest.mark.parametrize(
    "task",
    [
        {},
        {"tools": []},
        {"tools": [{"name": "other"}]},
        {
            "tool_endpoints": [
                {"tool_name": "lookup", "url": "http://fixture.invalid/undeclared"}
            ]
        },
    ],
)
def test_undeclared_tool_keeps_empty_endpoint(tmp_path, task):
    (row,) = dispatches(tmp_path, task, "lookup")
    assert row["tool_name"] == "lookup"
    assert row["endpoint_url"] == ""


@pytest.mark.parametrize(
    "command",
    [
        "mcporter call lookup value:12",
        "mcporter call claw-eval-fixture.lookup value:12",
        "mcporter call claw-eval-fixture lookup value:12",
    ],
)
def test_virtual_dispatch_shares_first_match_metadata(tmp_path, command):
    task = {
        "tools": [{"name": "lookup"}],
        "tool_endpoints": [
            {"tool_name": "lookup", "url": "http://fixture.invalid/first"},
            {"tool_name": "lookup", "url": "http://fixture.invalid/later"},
        ],
    }
    direct, virtual = dispatches(tmp_path, task, "exec", command)
    assert direct["endpoint_url"] == ""
    assert virtual["tool_name"] == "lookup"
    assert virtual["endpoint_url"] == "http://fixture.invalid/first"
    assert virtual["request_body"] == {"value": "12"}


def test_unknown_mcporter_tool_does_not_gain_virtual_dispatch(tmp_path):
    (row,) = dispatches(
        tmp_path,
        {"tools": [{"name": "lookup"}]},
        "exec",
        "mcporter call missing value:12",
    )
    assert row["tool_name"] == "exec"
