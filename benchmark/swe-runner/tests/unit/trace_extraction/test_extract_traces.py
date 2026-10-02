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

"""Tests for trace_extraction helpers and library entry points."""

import csv
import json

from swe_runner.trace_extraction import (
    analyze_trace_files,
    record_openclaw_jsonl_traces_in_window,
    write_trace_analysis_csvs,
)
from swe_runner.trace_extraction.helpers import (
    extract_issue_id,
    extract_user_text,
    ns_to_iso,
    parse_time_value,
    sanitize_path_component,
)
from swe_runner.trace_extraction.token_counting import count_tokens

# ---------------------------------------------------------------------------
# Unit tests for helpers
# ---------------------------------------------------------------------------


class TestNsToIso:
    def test_converts_nanoseconds(self):
        result = ns_to_iso(1_700_000_000_000_000_000)
        assert result.startswith("2023")
        assert "+00:00" in result or "Z" in result


class TestParseTimeValue:
    def test_parses_now(self):
        result = parse_time_value("now")
        assert isinstance(result, int)
        assert result > 0

    def test_parses_iso(self):
        assert parse_time_value("2026-04-21T10:45:00+08:00") == 1_776_739_500_000_000_000

    def test_parses_epoch_seconds(self):
        assert parse_time_value("10") == 10_000_000_000


class TestExtractIssueId:
    def test_finds_issue_id(self):
        text = "Some preamble\nIssue ID: django__django-12345\nMore text"
        assert extract_issue_id(text) == "django__django-12345"

    def test_returns_none_when_absent(self):
        assert extract_issue_id("no issue here") is None

    def test_returns_none_for_none_input(self):
        assert extract_issue_id(None) is None


class TestExtractUserText:
    def test_extracts_text(self):
        msgs = [{"role": "user", "parts": [{"type": "text", "content": "hello"}]}]
        assert extract_user_text(msgs) == "hello"

    def test_returns_none_when_no_user_msg(self):
        assert extract_user_text([]) is None


class TestSanitizePathComponent:
    def test_passes_safe_strings(self):
        assert sanitize_path_component("django__django-12345") == "django__django-12345"

    def test_replaces_unsafe_chars(self):
        assert sanitize_path_component("a/b c:d") == "a_b_c_d"

    def test_returns_unknown_for_empty(self):
        assert sanitize_path_component("") == "__unknown__"
        assert sanitize_path_component(None) == "__unknown__"


class TestRecordOpenClawJsonlTracesInWindow:
    def test_records_real_openclaw_message_content_shape(self, tmp_path):
        profiles_root = tmp_path / "openclaw-profiles"
        sessions_dir = profiles_root / "pydata__xarray-5131" / "agents" / "pydata__xarray-5131" / "sessions"
        sessions_dir.mkdir(parents=True)
        session_file = sessions_dir / "session-1.jsonl"
        session_file.write_text(
            "\n".join(
                [
                    json.dumps(
                        {
                            "type": "session",
                            "id": "session-1",
                            "timestamp": "2026-04-24T15:22:05.720Z",
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "user-1",
                            "timestamp": "2026-04-24T15:22:05.726Z",
                            "message": {
                                "role": "user",
                                "content": [
                                    {
                                        "type": "text",
                                        "text": "Repository: pydata/xarray\nIssue ID: pydata__xarray-5131\nBase Commit: abc",
                                    }
                                ],
                            },
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-1",
                            "timestamp": "2026-04-24T15:22:06.000Z",
                            "message": {
                                "role": "assistant",
                                "content": [
                                    {"type": "thinking", "thinking": "Need inspect."},
                                    {"type": "text", "text": "I will inspect the repo."},
                                    {"type": "toolCall", "id": "tc-1", "name": "exec", "arguments": {"cmd": "pytest"}},
                                ],
                                "usage": {"input": 100, "output": 25, "cacheRead": 10},
                                "model": "claude-sonnet-4",
                                "provider": "anthropic",
                            },
                        }
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )

        recorded = record_openclaw_jsonl_traces_in_window(
            parse_time_value("2026-04-24T15:22:00+00:00"),
            parse_time_value("2026-04-24T15:23:00+00:00"),
            profiles_root=profiles_root,
            trace_root=tmp_path / "traces",
        )

        assert [path.parent.name for path in recorded] == ["pydata__xarray-5131"]
        data = json.loads(recorded[0].read_text(encoding="utf-8"))
        assert data["initial_user_message"] == "Repository: pydata/xarray\nIssue ID: pydata__xarray-5131\nBase Commit: abc"
        assert data["issue_id"] == "pydata__xarray-5131"
        assert data["total_input_tokens"] == 100
        assert data["total_output_tokens"] == 25
        assert data["total_cache_read_tokens"] == 10
        assert data["models"] == ["claude-sonnet-4"]
        assert data["providers"] == ["anthropic"]
        assert data["steps"][0]["assistant_output"] == [
            {"type": "reasoning", "content": "Need inspect."},
            {"type": "text", "content": "I will inspect the repo."},
            {"type": "tool_call", "id": "tc-1", "name": "exec", "arguments": {"cmd": "pytest"}},
        ]

    def test_records_openclaw_assistant_steps_with_zero_usage(self, tmp_path):
        profiles_root = tmp_path / "openclaw-profiles"
        sessions_dir = profiles_root / "astropy__astropy-12907" / "agents" / "astropy__astropy-12907" / "sessions"
        sessions_dir.mkdir(parents=True)
        session_file = sessions_dir / "session-1.jsonl"
        session_file.write_text(
            "\n".join(
                [
                    json.dumps(
                        {
                            "type": "session",
                            "id": "session-1",
                            "timestamp": "2026-04-24T00:00:00+00:00",
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "user-1",
                            "timestamp": "2026-04-24T00:00:01+00:00",
                            "message": {
                                "role": "user",
                                "content": [{"type": "text", "text": "Issue ID: astropy__astropy-12907\n"}],
                            },
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-1",
                            "timestamp": "2026-04-24T00:00:02+00:00",
                            "message": {
                                "role": "assistant",
                                "content": [{"type": "text", "text": "I will inspect the repo."}],
                                "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                                "model": "qwen3.6-plus",
                                "provider": "bailian",
                            },
                        }
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )

        recorded = record_openclaw_jsonl_traces_in_window(
            parse_time_value("2026-04-24T00:00:00+00:00"),
            parse_time_value("2026-04-24T00:00:10+00:00"),
            profiles_root=profiles_root,
            trace_root=tmp_path / "traces",
        )

        assert [path.parent.name for path in recorded] == ["astropy__astropy-12907"]
        data = json.loads(recorded[0].read_text(encoding="utf-8"))
        assert data["total_steps"] == 1
        assert data["total_input_tokens"] == 0
        assert data["total_output_tokens"] == 0
        assert data["steps"][0]["assistant_output"] == [{"type": "text", "content": "I will inspect the repo."}]

    def test_records_session_jsonl_and_sums_usage(self, tmp_path):
        profiles_root = tmp_path / "openclaw-profiles"
        sessions_dir = profiles_root / "django__django-12345" / "agents" / "django__django-12345" / "sessions"
        sessions_dir.mkdir(parents=True)
        session_file = sessions_dir / "session-1.jsonl"
        session_file.write_text(
            "\n".join(
                [
                    json.dumps(
                        {
                            "type": "session",
                            "id": "session-1",
                            "timestamp": "2026-04-24T00:00:00+00:00",
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "user-1",
                            "role": "user",
                            "timestamp": "2026-04-24T00:00:01+00:00",
                            "parts": [
                                {
                                    "type": "text",
                                    "content": "Fix it\nIssue ID: django__django-12345\nThanks",
                                }
                            ],
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-1",
                            "role": "assistant",
                            "timestamp": "2026-04-24T00:00:02+00:00",
                            "model": "gpt-5.2",
                            "provider": "openai",
                                "usage": {
                                    "input": 100,
                                    "output": 25,
                                    "cacheRead": 10,
                                    "cacheWrite": 5,
                                    "reasoningTokens": 3,
                                    "totalTokens": 128,
                                    "cost": 0.001,
                                },
                            "parts": [{"type": "text", "content": "I will inspect the repo."}],
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-2",
                            "role": "assistant",
                            "timestamp": "2026-04-24T00:00:03+00:00",
                            "model": "gpt-5.2",
                            "provider": "openai",
                            "usage": {
                                "input_tokens": 200,
                                "output_tokens": 50,
                                "reasoning_tokens": 4,
                                "total_tokens": 254,
                            },
                            "parts": [
                                {"type": "tool_call", "id": "tc-1", "name": "exec", "arguments": {"cmd": "pytest"}},
                                {"type": "tool_call", "id": "tc-2", "name": "read", "arguments": {"path": "file.py"}},
                                {"type": "tool_call", "id": "tc-3", "name": "edit", "arguments": {"path": "file.py"}},
                            ],
                        }
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )

        recorded = record_openclaw_jsonl_traces_in_window(
            parse_time_value("2026-04-24T00:00:00+00:00"),
            parse_time_value("2026-04-24T00:00:10+00:00"),
            profiles_root=profiles_root,
            trace_root=tmp_path / "traces",
        )

        assert [path.parent.name for path in recorded] == ["django__django-12345"]
        data = json.loads(recorded[0].read_text(encoding="utf-8"))
        assert data["source"] == "openclaw-jsonl"
        assert data["session_id"] == "session-1"
        assert data["issue_id"] == "django__django-12345"
        assert data["total_input_tokens"] == 300
        assert data["total_output_tokens"] == 75
        assert data["total_cache_read_tokens"] == 10
        assert data["total_cache_write_tokens"] == 5
        assert data["total_reasoning_tokens"] == 7
        assert data["total_reported_tokens"] == 382
        assert data["max_step_input_tokens"] == 200
        assert data["max_step_output_tokens"] == 50
        assert data["models"] == ["gpt-5.2"]
        assert data["providers"] == ["openai"]
        assert data["total_steps"] == 2
        assert data["llm_turn_count"] == 2
        assert data["tool_call_count"] == 3
        assert data["tool_call_counts"] == {"edit": 1, "exec": 1, "read": 1}
        assert data["exec_command_count"] == 1
        assert data["pytest_command_count"] == 1
        assert data["file_read_tool_count"] == 1
        assert data["file_edit_tool_count"] == 1
        assert data["steps"][1]["tool_call_count"] == 3
        assert data["steps"][1]["assistant_output"][0]["name"] == "exec"
        assert data["steps"][0]["reasoning_tokens"] == 3
        assert data["steps"][1]["total_tokens"] == 254

    def test_attaches_openclaw_tool_results_to_next_model_step(self, tmp_path):
        profiles_root = tmp_path / "openclaw-profiles"
        sessions_dir = profiles_root / "astropy__astropy-12907" / "agents" / "astropy__astropy-12907" / "sessions"
        sessions_dir.mkdir(parents=True)
        session_file = sessions_dir / "session-1.jsonl"
        session_file.write_text(
            "\n".join(
                [
                    json.dumps(
                        {
                            "type": "session",
                            "id": "session-1",
                            "timestamp": "2026-04-24T00:00:00+00:00",
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "user-1",
                            "timestamp": "2026-04-24T00:00:01+00:00",
                            "message": {
                                "role": "user",
                                "content": [{"type": "text", "text": "Issue ID: astropy__astropy-12907\n"}],
                            },
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-1",
                            "timestamp": "2026-04-24T00:00:02+00:00",
                            "message": {
                                "role": "assistant",
                                "content": [
                                    {
                                        "type": "toolCall",
                                        "id": "call-1",
                                        "name": "exec",
                                        "arguments": {"command": "grep -n foo file.py"},
                                    }
                                ],
                                "usage": {"input": 100, "output": 25},
                                "model": "gpt-5.2",
                                "provider": "openai",
                            },
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "tool-1",
                            "timestamp": "2026-04-24T00:00:03+00:00",
                            "message": {
                                "role": "toolResult",
                                "toolCallId": "call-1",
                                "toolName": "exec",
                                "content": [{"type": "text", "text": "12: foo = 1"}],
                                "details": {
                                    "status": "completed",
                                    "exitCode": 0,
                                    "durationMs": 47,
                                    "aggregated": "12: foo = 1",
                                },
                                "isError": False,
                            },
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "tool-2",
                            "timestamp": "2026-04-24T00:00:03.500+00:00",
                            "message": {
                                "role": "toolResult",
                                "toolCallId": "call-err",
                                "toolName": "exec",
                                "content": [{"type": "text", "text": "boom"}],
                                "details": {
                                    "status": "failed",
                                    "exitCode": 2,
                                    "durationMs": 11,
                                    "aggregated": "boom",
                                },
                                "isError": True,
                            },
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-2",
                            "timestamp": "2026-04-24T00:00:04+00:00",
                            "message": {
                                "role": "assistant",
                                "content": [{"type": "text", "text": "Found it."}],
                                "usage": {"input": 200, "output": 50},
                                "model": "gpt-5.2",
                                "provider": "openai",
                            },
                        }
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )

        recorded = record_openclaw_jsonl_traces_in_window(
            parse_time_value("2026-04-24T00:00:00+00:00"),
            parse_time_value("2026-04-24T00:00:10+00:00"),
            profiles_root=profiles_root,
            trace_root=tmp_path / "traces",
        )

        data = json.loads(recorded[0].read_text(encoding="utf-8"))
        expected_response = "12: foo = 1"
        failed_response = "boom"
        assert data["tool_result_count"] == 2
        assert data["failed_tool_result_count"] == 1
        assert data["tool_result_counts"] == {"exec": 2}
        assert data["tool_result_chars"] == len(expected_response) + len(failed_response)
        assert data["tool_result_lines"] == 2
        assert data["tool_result_tokens_approx"] == count_tokens(expected_response) + count_tokens(failed_response)
        assert data["search_command_count"] == 1
        assert data["steps"][1]["tool_responses"] == [
            {
                "tool_call_id": "call-1",
                "tool_name": "exec",
                "response": expected_response,
                "is_error": False,
                "details": {
                    "status": "completed",
                    "exitCode": 0,
                    "durationMs": 47,
                    "aggregated": "12: foo = 1",
                },
            },
            {
                "tool_call_id": "call-err",
                "tool_name": "exec",
                "response": failed_response,
                "is_error": True,
                "details": {
                    "status": "failed",
                    "exitCode": 2,
                    "durationMs": 11,
                    "aggregated": failed_response,
                },
            }
        ]
        assert data["steps"][1]["tool_response_count"] == 2
        assert data["steps"][1]["failed_tool_response_count"] == 1
        assert data["steps"][1]["tool_response_chars"] == len(expected_response) + len(failed_response)
        assert data["steps"][1]["tool_response_tokens_approx"] == count_tokens(expected_response) + count_tokens(
            failed_response
        )

    def test_records_only_matching_openclaw_session_ids(self, tmp_path):
        profiles_root = tmp_path / "openclaw-profiles"
        sessions_dir = profiles_root / "django__django-12345" / "agents" / "django__django-12345" / "sessions"
        sessions_dir.mkdir(parents=True)

        def write_session(session_id: str) -> None:
            (sessions_dir / f"{session_id}.jsonl").write_text(
                "\n".join(
                    [
                        json.dumps(
                            {
                                "type": "session",
                                "id": session_id,
                                "timestamp": "2026-04-24T00:00:00+00:00",
                            }
                        ),
                        json.dumps(
                            {
                                "type": "message",
                                "id": f"user-{session_id}",
                                "role": "user",
                                "timestamp": "2026-04-24T00:00:01+00:00",
                                "parts": [{"type": "text", "content": "Issue ID: django__django-12345\n"}],
                            }
                        ),
                        json.dumps(
                            {
                                "type": "message",
                                "id": f"assistant-{session_id}",
                                "role": "assistant",
                                "timestamp": "2026-04-24T00:00:02+00:00",
                                "usage": {"input_tokens": 100, "output_tokens": 25},
                            }
                        ),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )

        write_session("session-keep")
        write_session("session-skip")

        recorded = record_openclaw_jsonl_traces_in_window(
            parse_time_value("2026-04-24T00:00:00+00:00"),
            parse_time_value("2026-04-24T00:00:10+00:00"),
            profiles_root=profiles_root,
            trace_root=tmp_path / "traces",
            session_ids={"session-keep"},
        )

        assert len(recorded) == 1
        data = json.loads(recorded[0].read_text(encoding="utf-8"))
        assert data["session_id"] == "session-keep"

    def test_records_matching_local_profile_dir_and_session_id(self, tmp_path):
        profiles_root = tmp_path / "openclaw-profiles"
        profile_dir = profiles_root / "astropy__astropy-12907"
        sessions_dir = profile_dir / "agents" / "astropy__astropy-12907" / "sessions"
        sessions_dir.mkdir(parents=True)
        session_id = "astropy__astropy-12907-a4c55ad4"
        (sessions_dir / f"{session_id}.jsonl").write_text(
            "\n".join(
                [
                    json.dumps(
                        {
                            "type": "session",
                            "id": session_id,
                            "timestamp": "2026-04-24T00:00:00+00:00",
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "user-1",
                            "role": "user",
                            "timestamp": "2026-04-24T00:00:01+00:00",
                            "parts": [{"type": "text", "content": "Fix this case.\n"}],
                        }
                    ),
                    json.dumps(
                        {
                            "type": "message",
                            "id": "assistant-1",
                            "role": "assistant",
                            "timestamp": "2026-04-24T00:00:02+00:00",
                            "usage": {"input_tokens": 100, "output_tokens": 25},
                        }
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )

        recorded = record_openclaw_jsonl_traces_in_window(
            parse_time_value("2026-04-24T00:00:00+00:00"),
            parse_time_value("2026-04-24T00:00:10+00:00"),
            profiles_root=None,
            profile_dirs=[profile_dir],
            trace_root=tmp_path / "traces",
            session_ids={session_id},
        )

        assert len(recorded) == 1
        data = json.loads(recorded[0].read_text(encoding="utf-8"))
        assert data["session_id"] == session_id
        assert data["issue_id"] == "astropy__astropy-12907"


class TestTraceAnalysis:
    def test_analyze_trace_files_returns_per_trace_and_summary_rows(self, tmp_path):
        trace_root = tmp_path / "traces"
        case_dir = trace_root / "django__django-12345"
        case_dir.mkdir(parents=True)

        (case_dir / "trace1.json").write_text(
            json.dumps(
                {
                    "session_id": "sess-1",
                    "models": ["claude-3-7-sonnet"],
                    "total_input_tokens": 100,
                    "total_output_tokens": 50,
                    "total_steps": 3,
                }
            ),
            encoding="utf-8",
        )
        (case_dir / "trace2.json").write_text(
            json.dumps(
                {
                    "session_id": "sess-2",
                    "models": ["claude-3-7-sonnet", "claude-3-5-haiku"],
                    "total_input_tokens": 200,
                    "total_output_tokens": 80,
                    "total_steps": 5,
                }
            ),
            encoding="utf-8",
        )

        per_trace_rows, per_instance_rows = analyze_trace_files(trace_root, trim_ratio=0.1)

        assert per_trace_rows[0]["instance_id"] == "django__django-12345"
        assert per_trace_rows[0]["task_id"] == "sess-1"
        assert per_trace_rows[0]["model"] == "claude-3-7-sonnet"
        assert per_trace_rows[0]["total_input_tokens"] == 100
        assert per_trace_rows[0]["total_output_tokens"] == 50
        assert per_trace_rows[0]["total_steps"] == 3
        assert "tool_call_count" not in per_trace_rows[0]
        assert "tool_call_counts" not in per_trace_rows[0]
        assert per_trace_rows[1]["task_id"] == "sess-2"
        assert per_trace_rows[1]["model"] == "claude-3-7-sonnet;claude-3-5-haiku"
        assert per_trace_rows[1]["total_input_tokens"] == 200
        assert per_trace_rows[1]["total_output_tokens"] == 80
        assert per_trace_rows[1]["total_steps"] == 5

        assert len(per_instance_rows) == 1
        summary = per_instance_rows[0]
        assert summary["instance_id"] == "django__django-12345"
        assert summary["execution_count"] == 2
        assert summary["avg_steps"] == "4.00"
        assert summary["min_steps"] == 3
        assert summary["max_steps"] == 5
        assert summary["avg_input_tokens"] == "150.00"
        assert summary["avg_output_tokens"] == "65.00"
        assert summary["avg_total_tokens"] == "215.00"
        assert summary["trimmed_avg_input_tokens"] == "150.00"
        assert summary["trimmed_avg_output_tokens"] == "65.00"
        assert summary["trimmed_avg_total_tokens"] == "215.00"
        assert summary["min_total_tokens"] == 150
        assert summary["max_total_tokens"] == 280
        assert "avg_tool_call_count" not in summary
        assert "tool_call_counts" not in summary

    def test_analyze_trace_files_can_include_metrics(self, tmp_path):
        trace_root = tmp_path / "traces"
        case_dir = trace_root / "django__django-12345"
        case_dir.mkdir(parents=True)
        (case_dir / "trace1.json").write_text(
            json.dumps(
                {
                    "session_id": "sess-1",
                    "models": ["claude-3-7-sonnet"],
                    "total_input_tokens": 100,
                    "total_output_tokens": 50,
                    "total_steps": 3,
                }
            ),
            encoding="utf-8",
        )

        per_trace_rows, per_instance_rows = analyze_trace_files(trace_root, trim_ratio=0.1, include_metrics=True)

        assert per_trace_rows[0]["tool_call_count"] == 0
        assert per_trace_rows[0]["tool_call_counts"] == "{}"
        assert per_trace_rows[0]["llm_turn_count"] == 0
        metrics_summary = per_instance_rows[0]
        assert metrics_summary["avg_tool_call_count"] == "0.00"
        assert metrics_summary["tool_call_counts"] == "{}"

    def test_analyze_trace_files_falls_back_to_step_models(self, tmp_path):
        trace_root = tmp_path / "traces"
        case_dir = trace_root / "django__django-99999"
        case_dir.mkdir(parents=True)
        (case_dir / "trace1.json").write_text(
            json.dumps(
                {
                    "session_id": "sess-step-model",
                    "steps": [
                        {"model": "claude-3-7-sonnet"},
                        {"model": "claude-3-7-sonnet"},
                        {"model": "claude-3-5-haiku"},
                    ],
                    "total_input_tokens": 10,
                    "total_output_tokens": 5,
                    "total_steps": 3,
                }
            ),
            encoding="utf-8",
        )

        per_trace_rows, _ = analyze_trace_files(trace_root, trim_ratio=0.1)

        assert len(per_trace_rows) == 1
        assert per_trace_rows[0]["instance_id"] == "django__django-99999"
        assert per_trace_rows[0]["task_id"] == "sess-step-model"
        assert per_trace_rows[0]["model"] == "claude-3-7-sonnet;claude-3-5-haiku"
        assert per_trace_rows[0]["total_input_tokens"] == 10
        assert per_trace_rows[0]["total_output_tokens"] == 5
        assert per_trace_rows[0]["total_steps"] == 3
        assert "llm_turn_count" not in per_trace_rows[0]

    def test_write_trace_analysis_csvs_writes_expected_files(self, tmp_path):
        trace_root = tmp_path / "traces"
        output_dir = tmp_path / "analysis"
        case_dir = trace_root / "astropy__astropy-1"
        case_dir.mkdir(parents=True)
        (case_dir / "trace1.json").write_text(
            json.dumps(
                {
                    "session_id": "sess-1",
                    "models": ["gpt-4.1"],
                    "total_input_tokens": 12,
                    "total_output_tokens": 8,
                    "total_steps": 2,
                }
            ),
            encoding="utf-8",
        )

        detail_dir, summary_csv = write_trace_analysis_csvs(trace_root=trace_root, output_dir=output_dir)

        assert detail_dir.exists()
        assert summary_csv.exists()
        detail_csv = detail_dir / "astropy__astropy-1.csv"
        metrics_csv = output_dir / "trace_metrics" / "trace_metrics.csv"
        assert detail_csv.exists()
        assert metrics_csv.exists()

        with open(detail_csv, encoding="utf-8", newline="") as f:
            detail_header = next(csv.reader(f))
        with open(summary_csv, encoding="utf-8", newline="") as f:
            summary_header = next(csv.reader(f))
        with open(metrics_csv, encoding="utf-8", newline="") as f:
            metrics_header = next(csv.reader(f))

        assert detail_header == ["用例ID", "任务ID", "模型", "总输入Token数", "总输出Token数", "总执行步数"]
        assert summary_header == [
            "用例ID",
            "执行次数",
            "平均执行步骤数",
            "最小执行步骤数",
            "最大执行步骤数",
            "平均输入Token数",
            "平均输出Token数",
            "平均总Token数",
            "截尾平均输入Token数",
            "截尾平均输出Token数",
            "截尾平均总Token数",
            "最小总Token数",
            "最大总Token数",
        ]
        assert "工具结果近似Token数" in metrics_header
        assert "工具结果近似Token数" not in detail_header
        assert "平均工具结果近似Token数" not in summary_header

    def test_analyze_trace_files_can_limit_to_selected_files(self, tmp_path):
        trace_root = tmp_path / "traces"
        keep_dir = trace_root / "keep__case-1"
        skip_dir = trace_root / "skip__case-2"
        keep_dir.mkdir(parents=True)
        skip_dir.mkdir(parents=True)
        keep_file = keep_dir / "trace1.json"
        keep_file.write_text(
            json.dumps(
                {
                    "session_id": "sess-keep",
                    "models": ["gpt-4.1"],
                    "total_input_tokens": 10,
                    "total_output_tokens": 5,
                    "total_steps": 2,
                }
            ),
            encoding="utf-8",
        )
        (skip_dir / "trace1.json").write_text(
            json.dumps(
                {
                    "session_id": "sess-skip",
                    "models": ["gpt-4.1"],
                    "total_input_tokens": 99,
                    "total_output_tokens": 88,
                    "total_steps": 7,
                }
            ),
            encoding="utf-8",
        )

        per_trace_rows, per_instance_rows = analyze_trace_files(trace_root, trace_files=[keep_file])

        assert len(per_trace_rows) == 1
        assert per_trace_rows[0]["instance_id"] == "keep__case-1"
        assert len(per_instance_rows) == 1
        assert per_instance_rows[0]["instance_id"] == "keep__case-1"

    def test_write_trace_analysis_csvs_clears_stale_detail_csvs(self, tmp_path):
        trace_root = tmp_path / "traces"
        output_dir = tmp_path / "analysis"
        case_dir = trace_root / "astropy__astropy-1"
        case_dir.mkdir(parents=True)
        trace_file = case_dir / "trace1.json"
        trace_file.write_text(
            json.dumps(
                {
                    "session_id": "sess-1",
                    "models": ["gpt-4.1"],
                    "total_input_tokens": 12,
                    "total_output_tokens": 8,
                    "total_steps": 2,
                }
            ),
            encoding="utf-8",
        )

        stale_detail_dir = output_dir / "trace_details"
        stale_detail_dir.mkdir(parents=True)
        stale_file = stale_detail_dir / "stale__case.csv"
        stale_file.write_text("stale", encoding="utf-8")

        detail_dir, _ = write_trace_analysis_csvs(trace_root=trace_root, output_dir=output_dir)

        assert detail_dir.exists()
        assert not stale_file.exists()
        assert (detail_dir / "astropy__astropy-1.csv").exists()
