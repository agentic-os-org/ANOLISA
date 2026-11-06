# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Tool responses keep all text evidence in conversation and dispatch records."""

import json
from pathlib import Path
from typing import Any

import pytest
from ce_runner.session_trace_converter import convert_session_to_trace


@pytest.mark.parametrize(
    ("content", "expected_blocks", "expected_body"),
    [
        (
            [
                {"type": "text", "text": "part one"},
                {"type": "text", "text": "part two"},
            ],
            [
                {"type": "text", "text": "part one"},
                {"type": "text", "text": "part two"},
            ],
            "part one\npart two",
        ),
        (
            [{"type": "text", "text": ""}, {"type": "text", "text": "later evidence"}],
            [{"type": "text", "text": ""}, {"type": "text", "text": "later evidence"}],
            "\nlater evidence",
        ),
        (
            [{"type": "text", "text": "only result"}],
            [{"type": "text", "text": "only result"}],
            "only result",
        ),
        ([], [{"type": "text", "text": ""}], ""),
    ],
)
def test_complete_ordered_text(
    tmp_path: Path,
    content: list[dict[str, Any]],
    expected_blocks: list[dict[str, str]],
    expected_body: str,
) -> None:
    converted = _convert(tmp_path, content)
    result = next(
        event["message"]["content"][0]
        for event in converted
        if event["type"] == "message"
        and event["message"]["content"][0]["type"] == "tool_result"
    )
    dispatch = next(event for event in converted if event["type"] == "tool_dispatch")
    assert result["content"] == expected_blocks
    assert dispatch["response_body"] == expected_body
    assert dispatch["response_status"] == 200


def test_error_response_retains_later_diagnostics(tmp_path: Path) -> None:
    converted = _convert(
        tmp_path,
        [
            {"type": "text", "text": "request failed"},
            {"type": "text", "text": "missing required account field"},
        ],
        is_error=True,
    )
    dispatch = next(event for event in converted if event["type"] == "tool_dispatch")
    assert dispatch["response_status"] == 500
    assert dispatch["response_body"] == "request failed\nmissing required account field"


@pytest.mark.parametrize("exec_command", [None, "mcporter call lookup account:42"])
def test_later_validation_error_still_records_http_status(
    tmp_path: Path, exec_command: str | None
) -> None:
    validation = {
        "detail": [
            {"type": "missing", "loc": ["body", "account"], "msg": "Field required"}
        ]
    }
    converted = _convert(
        tmp_path,
        [
            {"type": "text", "text": "service response follows"},
            {"type": "text", "text": json.dumps(validation)},
        ],
        exec_command=exec_command,
    )
    dispatches = [event for event in converted if event["type"] == "tool_dispatch"]
    assert len(dispatches) == (2 if exec_command else 1)
    assert all(dispatch["response_status"] == 422 for dispatch in dispatches)
    assert all("Field required" in dispatch["response_body"] for dispatch in dispatches)


def _convert(
    tmp_path: Path,
    content: list[dict[str, Any]],
    is_error: bool = False,
    exec_command: str | None = None,
) -> list[dict[str, Any]]:
    session = tmp_path / "session.jsonl"
    events = [
        {
            "type": "message",
            "timestamp": "2026-01-01T10:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "toolCall",
                        "id": "call-1",
                        "name": "exec" if exec_command else "lookup",
                        "arguments": {"command": exec_command} if exec_command else {},
                    }
                ],
            },
        },
        {
            "type": "message",
            "timestamp": "2026-01-01T10:00:01Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "call-1",
                "toolName": "lookup",
                "isError": is_error,
                "content": content,
            },
        },
    ]
    session.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    output = tmp_path / "trace.jsonl"
    convert_session_to_trace(
        str(session),
        {"task_id": "T001", "tools": [{"name": "lookup"}]},
        str(output),
        preloaded_audit_data={},
    )
    return [
        json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()
    ]
