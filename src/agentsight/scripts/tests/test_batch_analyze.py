"""Contract tests for the batch trajectory analyzer.

batch-analyze.py converts Claude/Qoder conversation JSONL into ATIF v1.7 and
drives the analyze binary over each file. The conversion is the subtle part:
message-content shapes vary (string or block lists), tool results must attach
as observations to the agent step that produced them, and usage metrics map
onto ATIF step metrics. A regression in any of those either loses trajectory
content or mislabels it, and the batch tool silently reports fewer steps.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "batch-analyze.py"

spec = importlib.util.spec_from_file_location("batch_analyze", SCRIPT)
batch_analyze = importlib.util.module_from_spec(spec)
sys.modules["batch_analyze"] = batch_analyze
spec.loader.exec_module(batch_analyze)


def write_jsonl(path: Path, entries: list) -> Path:
    path.write_text(
        "\n".join(json.dumps(entry, ensure_ascii=False) for entry in entries),
        encoding="utf-8",
    )
    return path


# ─── extract_text ─────────────────────────────────────────────────────────────


class TestExtractText:
    def test_a_plain_string_passes_through(self):
        assert batch_analyze.extract_text("hello") == "hello"

    def test_text_blocks_are_joined(self):
        content = [
            {"type": "text", "text": "one"},
            {"type": "text", "text": "two"},
        ]
        assert batch_analyze.extract_text(content) == "one\ntwo"

    def test_tool_result_content_is_included(self):
        content = [{"type": "tool_result", "content": "the output"}]
        assert batch_analyze.extract_text(content) == "the output"

    def test_bare_strings_in_a_block_list_are_kept(self):
        assert batch_analyze.extract_text(["plain", {"type": "text", "text": "block"}]) == (
            "plain\nblock"
        )

    def test_unknown_shapes_yield_the_empty_string(self):
        assert batch_analyze.extract_text(None) == ""
        assert batch_analyze.extract_text(42) == ""
        assert batch_analyze.extract_text([{"type": "image"}]) == ""


# ─── extract_tool_calls / extract_tool_results / extract_thinking ────────────


class TestExtractToolCalls:
    def test_tool_use_blocks_map_to_atif_calls(self):
        content = [
            {"type": "tool_use", "id": "tu-1", "name": "Bash", "input": {"command": "ls"}},
        ]
        assert batch_analyze.extract_tool_calls(content) == [
            {
                "tool_call_id": "tu-1",
                "function_name": "Bash",
                "arguments": {"command": "ls"},
            }
        ]

    def test_non_list_content_yields_no_calls(self):
        assert batch_analyze.extract_tool_calls("text only") == []


class TestExtractToolResults:
    def test_nested_result_content_is_flattened(self):
        content = [
            {
                "type": "tool_result",
                "tool_use_id": "tu-1",
                "content": [{"text": "line one"}, {"text": "line two"}],
            }
        ]
        results = batch_analyze.extract_tool_results(content)
        assert results == [{"source_call_id": "tu-1", "content": "line one line two"}]

    def test_result_content_is_truncated_to_two_thousand_chars(self):
        content = [{"type": "tool_result", "tool_use_id": "tu-1", "content": "x" * 3000}]
        results = batch_analyze.extract_tool_results(content)
        assert len(results[0]["content"]) == 2000


class TestExtractThinking:
    def test_thinking_blocks_are_joined(self):
        content = [
            {"type": "thinking", "thinking": "first"},
            {"type": "thinking", "thinking": "second"},
        ]
        assert batch_analyze.extract_thinking(content) == "first\nsecond"

    def test_no_thinking_yields_none(self):
        assert batch_analyze.extract_thinking([{"type": "text", "text": "hi"}]) is None
        assert batch_analyze.extract_thinking("plain") is None


# ─── jsonl_to_atif ────────────────────────────────────────────────────────────


class TestJsonlToAtif:
    def make_session(self, tmp_path: Path, name: str = "session-42") -> Path:
        return tmp_path / ".claude" / "projects" / f"{name}.jsonl"

    def test_user_and_agent_steps_are_numbered_and_sourced(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        write_jsonl(session, [
            {"type": "user", "message": {"role": "user", "content": "please list files"}},
            {
                "type": "assistant",
                "timestamp": "2026-01-02T03:04:05Z",
                "message": {
                    "role": "assistant",
                    "model": "claude-sonnet",
                    "content": [{"type": "text", "text": "listing files"}],
                },
            },
        ])

        atif = batch_analyze.jsonl_to_atif(session)

        assert atif["schema_version"] == "ATIF-v1.7"
        assert atif["session_id"] == "session-42"
        assert atif["agent"]["name"] == "claude-code"
        assert atif["agent"]["model_name"] == "claude-sonnet"
        assert [step["source"] for step in atif["steps"]] == ["user", "agent"]
        assert [step["step_id"] for step in atif["steps"]] == [1, 2]
        assert atif["steps"][0]["message"] == "please list files"
        assert atif["steps"][1]["message"] == "listing files"

    def test_tool_results_attach_to_the_previous_agent_step(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        write_jsonl(session, [
            {"type": "user", "message": {"role": "user", "content": "list files"}},
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "running ls"},
                        {"type": "tool_use", "id": "tu-1", "name": "Bash", "input": {}},
                    ],
                },
            },
            {
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "tu-1", "content": "file-a\nfile-b"}
                    ],
                },
            },
        ])

        atif = batch_analyze.jsonl_to_atif(session)

        # the tool result did not open a third step: it attached as the
        # observation of the agent step that called the tool
        assert len(atif["steps"]) == 2
        observation = atif["steps"][1]["observation"]["results"]
        assert observation == [{"source_call_id": "tu-1", "content": "file-a\nfile-b"}]

    def test_usage_metrics_map_onto_the_agent_step(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        write_jsonl(session, [
            {"type": "user", "message": {"role": "user", "content": "hi"}},
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "hello"}],
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 20,
                        "cache_read_input_tokens": 5,
                    },
                },
            },
        ])

        atif = batch_analyze.jsonl_to_atif(session)

        assert atif["steps"][1]["metrics"] == {
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "cached_tokens": 5,
        }

    def test_runtime_config_supplies_the_model(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        write_jsonl(session, [
            {"type": "runtime-config", "model": "qoder-x"},
            {"type": "user", "message": {"role": "user", "content": "hi"}},
            {
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "yo"}]},
            },
        ])

        atif = batch_analyze.jsonl_to_atif(session)

        # the assistant step carries the runtime-config model because the
        # message itself had none
        assert atif["steps"][1]["model_name"] == "qoder-x"

    def test_agent_name_comes_from_the_directory(self, tmp_path):
        qoder_session = tmp_path / ".qoder" / "projects" / "s1.jsonl"
        qoder_session.parent.mkdir(parents=True)
        write_jsonl(qoder_session, [
            {"type": "user", "message": {"role": "user", "content": "hi"}},
        ])
        plain_session = tmp_path / "exports" / "s2.jsonl"
        plain_session.parent.mkdir(parents=True)
        write_jsonl(plain_session, [
            {"type": "user", "message": {"role": "user", "content": "hi"}},
        ])

        assert batch_analyze.jsonl_to_atif(qoder_session)["agent"]["name"] == "qoder"
        assert batch_analyze.jsonl_to_atif(plain_session)["agent"]["name"] == "unknown"

    def test_noise_entries_are_skipped_and_bad_lines_tolerated(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        session.write_text(
            "\n".join([
                json.dumps({"type": "mode", "mode": "acceptEdits"}),
                json.dumps({"type": "summary", "summary": "old chat"}),
                "{ this line is not json",
                json.dumps({"type": "user", "message": {"role": "user", "content": "real"}}),
            ]),
            encoding="utf-8",
        )

        atif = batch_analyze.jsonl_to_atif(session)

        assert atif is not None
        assert len(atif["steps"]) == 1
        assert atif["steps"][0]["message"] == "real"

    def test_long_messages_are_truncated_to_five_thousand_chars(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        write_jsonl(session, [
            {"type": "user", "message": {"role": "user", "content": "x" * 6000}},
        ])

        atif = batch_analyze.jsonl_to_atif(session)

        assert len(atif["steps"][0]["message"]) == 5000

    def test_empty_or_stepless_files_return_none(self, tmp_path):
        session = self.make_session(tmp_path)
        session.parent.mkdir(parents=True)
        session.write_text("", encoding="utf-8")
        assert batch_analyze.jsonl_to_atif(session) is None

        write_jsonl(session, [{"type": "mode", "mode": "default"}])
        assert batch_analyze.jsonl_to_atif(session) is None


# ─── find_conversation_files ──────────────────────────────────────────────────


class TestFindConversationFiles:
    def test_filters_and_orders_by_recency(self, tmp_path):
        projects = tmp_path / "projects"
        projects.mkdir()
        old = projects / "old.jsonl"
        new = projects / "new.jsonl"
        tiny = projects / "tiny.jsonl"
        subagent = projects / "subagent-run.jsonl"
        for path, size in ((old, 2048), (new, 2048), (tiny, 100), (subagent, 2048)):
            path.write_text("x" * size, encoding="utf-8")
        os.utime(old, (1, 1))
        os.utime(new, (2, 2))

        found = batch_analyze.find_conversation_files([projects], min_size=1024)

        assert found == [new, old]

    def test_missing_directories_are_tolerated(self, tmp_path):
        found = batch_analyze.find_conversation_files([tmp_path / "nope"])
        assert found == []


# ─── main ─────────────────────────────────────────────────────────────────────


class TestMain:
    @staticmethod
    def make_session(tmp_path: Path, steps: int) -> Path:
        session = tmp_path / ".claude" / "projects" / "batch-session.jsonl"
        session.parent.mkdir(parents=True, exist_ok=True)
        entries = [{"type": "user", "message": {"role": "user", "content": "go"}}]
        for index in range(steps):
            entries.append({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": f"answer {index}"}],
                },
            })
        write_jsonl(session, entries)
        return session

    def run_main(self, monkeypatch, argv: list, run_result=None, run_error=None):
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            captured["kwargs"] = kwargs
            if run_error is not None:
                raise run_error
            return run_result

        monkeypatch.setattr(batch_analyze.subprocess, "run", fake_run)
        monkeypatch.setattr(sys, "argv", ["batch-analyze.py"] + argv)
        batch_analyze.main()
        return captured

    def test_analyzes_a_single_file_and_attributes_the_output(
        self, monkeypatch, capsys, tmp_path
    ):
        session = self.make_session(tmp_path, steps=3)
        analyze_output = {"dimension": "perf", "score": 0.9}

        captured = self.run_main(
            monkeypatch,
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(analyze_output), stderr=""
            ),
        )

        # the analyze binary receives the converted ATIF temp file and the dim
        assert captured["cmd"][0] == "/fake/analyze"
        assert captured["cmd"][2:] == ["--dim", "perf"]
        atif_path = Path(captured["cmd"][1])
        assert atif_path.exists() is False, "temp ATIF file must be cleaned up"
        stdout = capsys.readouterr().out
        report = json.loads(stdout)
        assert report["_source_file"] == str(session)
        assert report["_session_id"] == "batch-session"
        assert report["_steps"] == 4
        assert report["score"] == 0.9

    def test_conversations_below_the_step_floor_are_skipped(
        self, monkeypatch, capsys, tmp_path
    ):
        session = self.make_session(tmp_path, steps=1)

        self.run_main(
            monkeypatch,
            ["--dim", "cost", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(args=[], returncode=0, stdout="{}", stderr=""),
        )

        err = capsys.readouterr().err
        assert "SKIP" in err
        assert "0 analyzed" in err

    def test_analyze_failures_are_counted_not_raised(self, monkeypatch, capsys, tmp_path):
        session = self.make_session(tmp_path, steps=3)

        self.run_main(
            monkeypatch,
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(
                args=[], returncode=1, stdout="", stderr="boom"
            ),
        )

        assert "1 failed" in capsys.readouterr().err

    def test_invalid_analyze_output_is_counted_not_raised(
        self, monkeypatch, capsys, tmp_path
    ):
        session = self.make_session(tmp_path, steps=3)

        self.run_main(
            monkeypatch,
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(args=[], returncode=0, stdout="not-json", stderr=""),
        )

        assert "1 failed" in capsys.readouterr().err

    def test_analyze_timeouts_are_counted_not_raised(self, monkeypatch, capsys, tmp_path):
        session = self.make_session(tmp_path, steps=3)

        self.run_main(
            monkeypatch,
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_error=subprocess.TimeoutExpired(cmd=[], timeout=300),
        )

        assert "TIMEOUT" in capsys.readouterr().err
