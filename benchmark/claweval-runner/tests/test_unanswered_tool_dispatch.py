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

"""Interrupted tool attempts remain visible alongside completed dispatches."""

import json
from pathlib import Path
from typing import Any

import pytest
from ce_runner.session_trace_converter import convert_session_to_trace


@pytest.mark.parametrize("is_exec", [False, True])
def test_unanswered_call_is_recorded_as_failed(tmp_path: Path, is_exec: bool) -> None:
    call = {
        "type": "toolCall",
        "id": "pending-1",
        "name": "exec" if is_exec else "lookup",
        "arguments": {"command": "mcporter call lookup account:42"}
        if is_exec
        else {"account": 42},
    }
    events, metadata = _convert(tmp_path, [_message("assistant", [call], 0)])
    dispatches = [event for event in events if event["type"] == "tool_dispatch"]
    assert len(dispatches) == (2 if is_exec else 1)
    assert metadata["tool_dispatches"] == len(dispatches)
    assert all(event["response_status"] == 500 for event in dispatches)
    assert all(event["response_body"] == "" for event in dispatches)
    assert all(
        event["timestamp"] == "2026-01-01T10:00:00+00:00" for event in dispatches
    )
    assert events[-1]["total_turns"] == 1


def test_mixed_calls_preserve_completed_result_and_pending_failure(
    tmp_path: Path,
) -> None:
    calls = [
        {
            "type": "toolCall",
            "id": name,
            "name": "lookup",
            "arguments": {"account": index},
        }
        for index, name in enumerate(["completed", "pending-a", "pending-b"])
    ]
    result = _message("toolResult", [{"type": "text", "text": "found"}], 1)
    result["message"]["toolCallId"] = "completed"
    events, metadata = _convert(tmp_path, [_message("assistant", calls, 0), result])
    dispatches = {
        event["tool_use_id"]: event
        for event in events
        if event["type"] == "tool_dispatch"
    }
    assert set(dispatches) == {"completed", "pending-a", "pending-b"}
    assert dispatches["completed"]["response_status"] == 200
    assert dispatches["completed"]["response_body"] == "found"
    assert dispatches["pending-a"]["response_status"] == 500
    assert dispatches["pending-b"]["request_body"] == {"account": 2}
    assert metadata["tool_dispatches"] == 3


def test_pending_dispatch_uses_call_position_before_later_final_answer(
    tmp_path: Path,
) -> None:
    call = {"type": "toolCall", "id": "pending", "name": "lookup", "arguments": {}}
    events, _ = _convert(
        tmp_path,
        [
            _message("assistant", [call], 0),
            _message("assistant", [{"type": "text", "text": "Final answer"}], 2),
        ],
    )
    body = [event for event in events if event["type"] in {"message", "tool_dispatch"}]
    assert [event["type"] for event in body] == ["message", "tool_dispatch", "message"]
    assert events[-1]["final_text"] == "Final answer"


def test_completed_call_is_not_emitted_again_at_session_end(tmp_path: Path) -> None:
    call = {"type": "toolCall", "id": "completed", "name": "lookup", "arguments": {}}
    result = _message("toolResult", [{"type": "text", "text": "found"}], 1)
    result["message"]["toolCallId"] = "completed"
    events, metadata = _convert(tmp_path, [_message("assistant", [call], 0), result])
    dispatches = [event for event in events if event["type"] == "tool_dispatch"]
    assert len(dispatches) == metadata["tool_dispatches"] == 1
    assert dispatches[0]["response_status"] == 200


def _message(role: str, content: list[dict[str, Any]], second: int) -> dict[str, Any]:
    return {
        "type": "message",
        "timestamp": f"2026-01-01T10:00:0{second}Z",
        "message": {"role": role, "content": content},
    }


def _convert(
    tmp_path: Path, events: list[dict[str, Any]]
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    session = tmp_path / "session.jsonl"
    session.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    output = tmp_path / "trace.jsonl"
    task = {"task_id": "T001", "tools": [{"name": "lookup"}]}
    metadata = convert_session_to_trace(
        str(session), task, str(output), preloaded_audit_data={}
    )
    return [
        json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()
    ], metadata
