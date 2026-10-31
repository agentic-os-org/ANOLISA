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

"""Virtual requests should reflect shell argument boundaries without execution."""

import json
from pathlib import Path

import pytest
from ce_runner.session_trace_converter import convert_session_to_trace


@pytest.mark.parametrize(
    ("command", "parameters"),
    [
        (
            'mcporter call lookup query:"two words" limit:10',
            {"query": "two words", "limit": "10"},
        ),
        (
            "mcporter call claw-eval-demo.lookup query:'two words'",
            {"query": "two words"},
        ),
        (
            'mcporter call claw-eval-demo lookup query:"two words"',
            {"query": "two words"},
        ),
        (
            'mcporter call --config "/tmp/my config.json" lookup query:"two words"',
            {"query": "two words"},
        ),
        ('mcporter call "lookup" query:"two words"', {"query": "two words"}),
        (r"mcporter call lookup query:two\ words", {"query": "two words"}),
        (
            'mcporter call lookup url:"https://example.test/a?x=1&y=2" note:"one;two"',
            {"url": "https://example.test/a?x=1&y=2", "note": "one;two"},
        ),
        ("mcporter call lookup query:first && echo leaked:later", {"query": "first"}),
        (
            "/usr/bin/mcporter call --config /tmp/config.json lookup query:plain limit:2",
            {"query": "plain", "limit": "2"},
        ),
    ],
)
def test_virtual_request_keeps_shell_parameters(
    tmp_path: Path, command: str, parameters: dict[str, str]
) -> None:
    dispatches = _dispatches(tmp_path, command)
    virtual = [event for event in dispatches if event["tool_name"] == "lookup"]
    assert len(virtual) == 1
    assert virtual[0]["request_body"] == parameters
    assert virtual[0]["endpoint_url"] == "http://localhost:9000/lookup"
    assert virtual[0]["response_body"] == "found"
    assert virtual[0]["response_status"] == 200


@pytest.mark.parametrize(
    "command",
    [
        'echo "mcporter call lookup query:invented"',
        'echo data > mcporter call lookup query:invented',
        'mcporter call lookup query:"unterminated',
        "mcporter call missing query:ignored",
        "mcporter call --config",
        "printf lookup",
    ],
)
def test_unrecognized_commands_keep_only_original_exec_dispatch(
    tmp_path: Path, command: str
) -> None:
    dispatches = _dispatches(tmp_path, command)
    assert len(dispatches) == 1
    assert dispatches[0]["tool_name"] == "exec"


def _dispatches(tmp_path: Path, command: str) -> list[dict]:
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
                        "arguments": {"command": command},
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
                "content": [{"type": "text", "text": "found"}],
            },
        },
    ]
    session.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    output = tmp_path / "trace.jsonl"
    task = {
        "task_id": "T001",
        "tools": [{"name": "lookup"}],
        "tool_endpoints": [
            {"tool_name": "lookup", "url": "http://localhost:9000/lookup"}
        ],
    }
    convert_session_to_trace(str(session), task, str(output), preloaded_audit_data={})
    return [
        event
        for line in output.read_text(encoding="utf-8").splitlines()
        if (event := json.loads(line))["type"] == "tool_dispatch"
    ]
