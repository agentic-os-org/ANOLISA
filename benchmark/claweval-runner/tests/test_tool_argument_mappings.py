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

"""Tool parameter normalization must produce mappings for all representations."""

import json
from pathlib import Path
from typing import Any

import pytest
from ce_runner.session_trace_converter import (
    convert_session_to_trace,
    parse_openclaw_arguments,
)


@pytest.mark.parametrize("value", [None, [], 42, True, "text"])
@pytest.mark.parametrize("wrapped", [False, True])
def test_non_object_json_becomes_empty_parameters(value: Any, wrapped: bool) -> None:
    encoded = json.dumps(value)
    arguments = {"kwargs": encoded} if wrapped else encoded
    assert parse_openclaw_arguments(arguments) == {}


@pytest.mark.parametrize("wrapper_value_is_json", [False, True])
def test_stringified_kwargs_wrapper_keeps_actual_parameters(
    wrapper_value_is_json: bool,
) -> None:
    parameters = {"command": "echo hello", "nested": {"items": [1, None, "two"]}}
    wrapper_value = json.dumps(parameters) if wrapper_value_is_json else parameters
    arguments = json.dumps({"kwargs": wrapper_value})
    assert parse_openclaw_arguments(arguments) == parameters


@pytest.mark.parametrize("arguments", ["not json", {"kwargs": "not json"}, None, 1, []])
def test_existing_invalid_argument_fallback(arguments: Any) -> None:
    assert parse_openclaw_arguments(arguments) == {}


def test_exec_result_with_non_object_arguments_does_not_abort_conversion(
    tmp_path: Path,
) -> None:
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
                        "id": "exec-1",
                        "name": "exec",
                        "arguments": "null",
                    }
                ],
            },
        },
        {
            "type": "message",
            "timestamp": "2026-01-01T10:00:01Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "exec-1",
                "toolName": "exec",
                "content": [{"type": "text", "text": "no command"}],
            },
        },
    ]
    session.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    output = tmp_path / "trace.jsonl"

    metadata = convert_session_to_trace(
        str(session), {"task_id": "T001"}, str(output), preloaded_audit_data={}
    )

    converted = [
        json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()
    ]
    dispatch = next(event for event in converted if event["type"] == "tool_dispatch")
    assert dispatch["request_body"] == {}
    assert dispatch["response_body"] == "no command"
    assert metadata["tool_dispatches"] == 1
    assert converted[-1]["type"] == "trace_end"
