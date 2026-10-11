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

"""Tool response metrics are preserved without repeated tokenization."""

import json
from pathlib import Path
from unittest.mock import patch

from swe_runner.trace_extraction.openclaw_jsonl import reconstruct_openclaw_jsonl_session


def test_response_metrics_tokenize_once_and_merge_trailing_results(tmp_path: Path) -> None:
    entries = [
        {"role": "assistant", "usage": {"input": 10, "output": 2}},
        {"role": "toolResult", "content": "first\nline", "toolName": "read", "details": {"exitCode": 1}},
        {"role": "toolResult", "content": "", "toolName": "read"},
        {"role": "assistant", "usage": {"input": 20, "output": 3}},
        {"role": "toolResult", "content": "last", "toolName": "exec", "isError": True},
    ]
    session_file = tmp_path / "session.jsonl"
    session_file.write_text("\n".join(json.dumps(entry) for entry in entries), encoding="utf-8")

    with patch("swe_runner.trace_extraction.openclaw_jsonl.count_tokens", side_effect=len) as tokenizer:
        trace = reconstruct_openclaw_jsonl_session(session_file)

    assert trace is not None
    assert trace["tool_result_count"] == 3
    assert trace["failed_tool_result_count"] == 2
    assert trace["tool_result_chars"] == 14
    assert trace["tool_result_lines"] == 3
    assert trace["tool_result_tokens_approx"] == 14
    first, last = trace["steps"]
    assert first["tool_response_count"] == 0
    assert first["tool_response_tokens_approx"] == 0
    assert last["tool_response_count"] == 3
    assert last["failed_tool_response_count"] == 2
    assert last["tool_response_chars"] == 14
    assert last["tool_response_lines"] == 3
    assert last["tool_response_tokens_approx"] == 14
    assert [item["response"] for item in last["tool_responses"]] == ["first\nline", "", "last"]
    assert [call.args[0] for call in tokenizer.call_args_list] == ["first\nline", "", "last"]
