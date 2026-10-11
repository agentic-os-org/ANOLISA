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

"""Token metrics accept arbitrary literal transcript text."""

import json
from pathlib import Path

import pytest
import tiktoken
from swe_runner.trace_extraction.openclaw_jsonl import reconstruct_openclaw_jsonl_session
from swe_runner.trace_extraction.token_counting import count_tokens


@pytest.mark.parametrize("text", ["", "plain text", "你好 🌍", "<|endoftext|>", "a <|fim_prefix|> b"])
def test_count_tokens_encodes_transcript_as_ordinary_text(text: str) -> None:
    expected = len(tiktoken.get_encoding("cl100k_base").encode_ordinary(text))
    assert count_tokens(text) == expected


def test_reconstruct_session_with_literal_special_token_in_tool_output(tmp_path: Path) -> None:
    response = "The fixture contains <|endoftext|> and <|fim_suffix|>."
    entries = [
        {
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [{"type": "toolCall", "id": "read-1", "name": "read", "arguments": {}}],
                "usage": {"input": 12, "output": 4},
            },
        },
        {
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": "read-1",
                "toolName": "read",
                "content": [{"type": "text", "text": response}],
            },
        },
    ]
    session_file = tmp_path / "session.jsonl"
    session_file.write_text("\n".join(json.dumps(entry) for entry in entries), encoding="utf-8")

    trace = reconstruct_openclaw_jsonl_session(session_file)

    assert trace is not None
    assert trace["tool_result_count"] == 1
    assert trace["tool_result_tokens_approx"] == len(tiktoken.get_encoding("cl100k_base").encode_ordinary(response))
    assert trace["steps"][0]["tool_responses"][0]["response"] == response
