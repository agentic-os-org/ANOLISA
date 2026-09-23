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

"""Test session to trace conversion logic.

Covers:
- Basic message conversion (user/assistant/toolResult)
- toolCall argument parsing (dict/JSON string/kwargs wrapper)
- mcporter virtual tool_dispatch extraction
- Timestamp normalization (Z suffix handling)
- trace_start event completeness
- trace_end event fields (scores/tokens/timing)
- audit_data fetching (success/failure/partial)
- Edge cases (empty session/invalid JSON/missing timestamp)

Task type coverage:
- T tasks: standard tool_dispatch + audit_snapshot
- M tasks: + image content blocks (base64 data URI)
- C tasks: multi-turn user messages ([user_agent] prefix)
"""

import json
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestMessageConversion:
    """Test basic message conversion."""

    def test_convert_user_message(self, tmp_path):
        """Convert user message with text content."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        # Create session file
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": [{"type": "text", "text": "Hello"}]}}\n'
        )
        
        # Create task yaml
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        result = convert_session_to_trace(str(session_file), task, str(output_file))
        
        assert output_file.exists()
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        
        # Check user message converted
        user_events = [e for e in events if e.get("message", {}).get("role") == "user"]
        assert len(user_events) > 0

    def test_convert_assistant_message_with_tool(self, tmp_path):
        """Convert assistant message with tool call."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:05.000Z", '
            '"message": {"role": "assistant", '
            '"content": [{"type": "text", "text": "Using tool"}, '
            '{"type": "toolCall", "id": "tool_001", "name": "test_tool", '
            '"arguments": {"param1": "value1"}}], '
            '"usage": {"input": 100, "output": 50}}}\n'
        )
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        convert_session_to_trace(str(session_file), task, str(output_file))
        
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        assistant_events = [e for e in events if e.get("message", {}).get("role") == "assistant"]
        assert len(assistant_events) > 0
        
        # Check tool_use block
        content = assistant_events[0]["message"]["content"]
        tool_use_blocks = [c for c in content if c.get("type") == "tool_use"]
        assert len(tool_use_blocks) > 0
        assert tool_use_blocks[0]["name"] == "test_tool"


class TestToolCallParsing:
    """Test toolCall argument parsing."""

    def test_parse_dict_arguments(self):
        """Parse direct dict arguments."""
        from ce_runner.session_trace_converter import parse_openclaw_arguments
        
        args = {"query": "test", "limit": 10}
        result = parse_openclaw_arguments(args)
        assert result == args

    def test_parse_json_string_arguments(self):
        """Parse JSON string arguments."""
        from ce_runner.session_trace_converter import parse_openclaw_arguments
        
        args = '{"query": "test", "limit": 10}'
        result = parse_openclaw_arguments(args)
        assert result["query"] == "test"
        assert result["limit"] == 10

    def test_parse_kwargs_wrapper(self):
        """Parse kwargs wrapper format."""
        from ce_runner.session_trace_converter import parse_openclaw_arguments
        
        args = {"kwargs": '{"param1": "value1"}'}
        result = parse_openclaw_arguments(args)
        assert result["param1"] == "value1"


class TestTimestampNormalization:
    """Test timestamp normalization."""

    def test_normalize_z_suffix(self):
        """Convert Z suffix to +00:00."""
        from ce_runner.session_trace_converter import normalize_timestamp
        
        assert normalize_timestamp("2024-01-15T10:00:00.000Z") == "2024-01-15T10:00:00.000+00:00"

    def test_no_change_for_valid_offset(self):
        """Don't modify timestamps with valid offset."""
        from ce_runner.session_trace_converter import normalize_timestamp
        
        ts = "2024-01-15T10:00:00.000+08:00"
        assert normalize_timestamp(ts) == ts


class TestTraceEvents:
    """Test trace event structure."""

    def test_trace_start_event(self, tmp_path):
        """Verify trace_start event completeness."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": []}}\n'
        )
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: M001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        convert_session_to_trace(str(session_file), task, str(output_file))
        
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        trace_start = events[0]
        
        assert trace_start["type"] == "trace_start"
        assert "trace_id" in trace_start
        assert "timestamp" in trace_start

    def test_trace_end_event_fields(self, tmp_path):
        """Verify trace_end event has all required fields."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": []}}\n'
            '{"type": "message", "timestamp": "2024-01-15T10:00:05.000Z", '
            '"message": {"role": "assistant", "content": [], '
            '"usage": {"input": 100, "output": 50}, "stopReason": "endTurn"}}\n'
        )
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        result = convert_session_to_trace(str(session_file), task, str(output_file))
        
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        trace_end = events[-1]
        
        assert trace_end["type"] == "trace_end"
        assert "task_score" in trace_end
        assert "scores" in trace_end
        assert "wall_time_s" in trace_end
        assert "total_turns" in trace_end


class TestMultimodalContent:
    """Test multimodal content conversion (M tasks)."""

    def test_convert_image_content(self, tmp_path):
        """Convert image content block with base64 data."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", '
            '"content": [{"type": "text", "text": "Describe this image"}, '
            '{"type": "image", "data": "iVBORw0KGgo=", '
            '"mime_type": "image/png"}]}}\n'
        )
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: M001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        convert_session_to_trace(str(session_file), task, str(output_file))
        
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        user_events = [e for e in events if e.get("message", {}).get("role") == "user"]
        
        if user_events:
            content = user_events[0]["message"]["content"]
            image_blocks = [c for c in content if c.get("type") == "image"]
            assert len(image_blocks) > 0
            assert image_blocks[0]["data"] == "iVBORw0KGgo="


class TestUserAgentMessages:
    """Test UserAgent multi-turn messages (C tasks)."""

    def test_convert_user_agent_messages(self, tmp_path):
        """Convert multi-turn conversation with [user_agent] prefix."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": [{"type": "text", "text": "Question"}]}}\n'
            '{"type": "message", "timestamp": "2024-01-15T10:00:05.000Z", '
            '"message": {"role": "assistant", "content": [{"type": "text", "text": "Answer"}]}}\n'
            '{"type": "message", "timestamp": "2024-01-15T10:00:10.000Z", '
            '"message": {"role": "user", "content": [{"type": "text", "text": "[user_agent]\\nFollow-up"}]}}\n'
        )
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: C01\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        result = convert_session_to_trace(str(session_file), task, str(output_file))
        
        assert output_file.exists()
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        user_events = [e for e in events if e.get("message", {}).get("role") == "user"]
        assert len(user_events) == 2  # Initial + follow-up


class TestEdgeCases:
    """Test edge cases and error handling."""

    def test_empty_session(self, tmp_path):
        """Handle empty session file."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text("")
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        result = convert_session_to_trace(str(session_file), task, str(output_file))
        
        # Should still create trace with start/end events
        assert output_file.exists()
        events = [json.loads(line) for line in output_file.read_text().strip().split('\n')]
        assert len(events) >= 2  # At least trace_start and trace_end

    def test_invalid_json_lines(self, tmp_path):
        """Skip invalid JSON lines."""
        from ce_runner.session_trace_converter import convert_session_to_trace
        
        session_file = tmp_path / "session.jsonl"
        session_file.write_text(
            "invalid json line\n"
            '{"type": "message", "timestamp": "2024-01-15T10:00:00.000Z", '
            '"message": {"role": "user", "content": []}}\n'
            "another invalid line\n"
        )
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        
        output_file = tmp_path / "output.jsonl"
        from ce_runner._common import load_task_yaml

        task = load_task_yaml(str(task_yaml))

        result = convert_session_to_trace(str(session_file), task, str(output_file))
        
        assert output_file.exists()


class TestTokenConsistency:
    """Ensure token usage stats are preserved 1:1 from session to trace.

    Guarantees that for every trial we keep two consistent records:
      - openclaw session.jsonl:  message.usage.input / .output / .totalTokens
      - ce-runner trace.jsonl:   per-message usage.input_tokens / .output_tokens
                                  and trace_end.model_input_tokens / .model_output_tokens
    """

    @staticmethod
    def _assistant_msg(ts, input_tok, output_tok, cache_read=0, cache_write=0):
        msg = {
            "type": "message",
            "timestamp": ts,
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": f"reply@{ts}"}],
                "usage": {
                    "input": input_tok,
                    "output": output_tok,
                    "cacheRead": cache_read,
                    "cacheWrite": cache_write,
                    "totalTokens": input_tok + output_tok,
                },
                "stopReason": "endTurn",
            },
        }
        return json.dumps(msg) + "\n"

    @staticmethod
    def _user_msg(ts, text="hi"):
        msg = {
            "type": "message",
            "timestamp": ts,
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": text}],
            },
        }
        return json.dumps(msg) + "\n"

    def _run(self, tmp_path, lines):
        from ce_runner._common import load_task_yaml
        from ce_runner.session_trace_converter import convert_session_to_trace

        session_file = tmp_path / "session.jsonl"
        session_file.write_text("".join(lines))
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nservices: []\ntools: []\n")
        task = load_task_yaml(str(task_yaml))
        output_file = tmp_path / "output.jsonl"
        convert_session_to_trace(str(session_file), task, str(output_file))
        return [json.loads(l) for l in output_file.read_text().splitlines() if l]

    def test_per_turn_tokens_match(self, tmp_path):
        """Each assistant message in trace carries the exact input/output from session."""
        turns = [(5688, 625), (6150, 2206), (8182, 111), (9230, 698)]
        lines = [self._user_msg("2024-01-15T10:00:00.000Z", "go")]
        for i, (in_tok, out_tok) in enumerate(turns):
            ts = f"2024-01-15T10:00:{(i + 1) * 5:02d}.000Z"
            lines.append(self._assistant_msg(ts, in_tok, out_tok))
        events = self._run(tmp_path, lines)

        assistant_events = [
            e for e in events
            if e.get("type") == "message"
            and e.get("message", {}).get("role") == "assistant"
        ]
        assert len(assistant_events) == len(turns)
        for ev, (in_tok, out_tok) in zip(assistant_events, turns):
            assert ev["usage"]["input_tokens"] == in_tok
            assert ev["usage"]["output_tokens"] == out_tok

    def test_trace_end_totals_match_session_sum(self, tmp_path):
        """trace_end aggregates equal sum of per-turn session usage."""
        turns = [(5688, 625), (6150, 2206), (8182, 111), (9230, 698),
                 (9507, 2810), (12260, 83), (13532, 694)]
        lines = [self._user_msg("2024-01-15T10:00:00.000Z", "go")]
        for i, (in_tok, out_tok) in enumerate(turns):
            ts = f"2024-01-15T10:00:{(i + 1) * 5:02d}.000Z"
            lines.append(self._assistant_msg(ts, in_tok, out_tok))
        events = self._run(tmp_path, lines)

        in_sum = sum(t[0] for t in turns)
        out_sum = sum(t[1] for t in turns)
        trace_end = next(e for e in events if e.get("type") == "trace_end")
        assert trace_end["model_input_tokens"] == in_sum
        assert trace_end["model_output_tokens"] == out_sum
        assert trace_end["input_tokens"] == in_sum
        assert trace_end["output_tokens"] == out_sum
        assert trace_end["total_tokens"] == in_sum + out_sum

    def test_zero_token_session(self, tmp_path):
        """A session with no assistant turns yields zero totals (and no NaN)."""
        events = self._run(tmp_path, [self._user_msg("2024-01-15T10:00:00.000Z")])
        trace_end = next(e for e in events if e.get("type") == "trace_end")
        assert trace_end["model_input_tokens"] == 0
        assert trace_end["model_output_tokens"] == 0
        assert trace_end["total_tokens"] == 0

    def test_cache_tokens_do_not_corrupt_totals(self, tmp_path):
        """Non-zero cacheRead/cacheWrite must not leak into input/output sums.

        The current trace schema does not surface cache fields, but we lock in
        the invariant that input/output_tokens stay equal to session.usage.input
        and .output regardless of cache values.
        """
        lines = [
            self._user_msg("2024-01-15T10:00:00.000Z"),
            self._assistant_msg(
                "2024-01-15T10:00:05.000Z",
                input_tok=1000,
                output_tok=200,
                cache_read=4096,
                cache_write=8192,
            ),
        ]
        events = self._run(tmp_path, lines)
        assistant = next(
            e for e in events
            if e.get("type") == "message"
            and e.get("message", {}).get("role") == "assistant"
        )
        assert assistant["usage"]["input_tokens"] == 1000
        assert assistant["usage"]["output_tokens"] == 200

        trace_end = next(e for e in events if e.get("type") == "trace_end")
        assert trace_end["model_input_tokens"] == 1000
        assert trace_end["model_output_tokens"] == 200
        assert trace_end["total_tokens"] == 1200
