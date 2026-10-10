"""Contract and regression tests for the batch trajectory analyzer.

``batch-analyze.py`` converts Claude/Qoder conversation JSONL into ATIF v1.7
and drives the analyze binary over each file. The conversion is the subtle
part: message-content shapes vary (string or block lists), tool results must
attach as observations to the agent step that produced them, and usage metrics
map onto ATIF step metrics. A regression in any of those either loses
trajectory content or mislabels it, and the batch tool silently reports fewer
steps.

The first three tests pin a fixed regression: ``jsonl_to_atif`` used to assume
every parsed JSONL line is an object, so a scalar or array line raised
``AttributeError`` and aborted the whole batch instead of being skipped like a
decode error.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).parents[2] / "batch-analyze.py"


def load_module():
    spec = importlib.util.spec_from_file_location("batch_analyze", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


batch_analyze = load_module()


def write_jsonl(path: Path, entries: list) -> Path:
    path.write_text(
        "\n".join(json.dumps(entry, ensure_ascii=False) for entry in entries),
        encoding="utf-8",
    )
    return path


# ─── regressions: non-object JSONL lines ──────────────────────────────────────


def test_scalar_line_is_skipped_like_a_decode_error():
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text(
            '42\n{"type":"user","message":{"role":"user","content":"hello"}}\n',
            encoding="utf-8",
        )
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    assert [step["message"] for step in doc["steps"]] == ["hello"]


def test_array_line_is_skipped_like_a_decode_error():
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text(
            '[1, 2]\n{"type":"user","message":{"role":"user","content":"hi"}}\n',
            encoding="utf-8",
        )
        doc = batch_analyze.jsonl_to_atif(session)
    assert doc is not None
    assert [step["message"] for step in doc["steps"]] == ["hi"]


def test_document_with_only_scalars_is_rejected():
    with tempfile.TemporaryDirectory() as tmp:
        session = Path(tmp) / "session.jsonl"
        session.write_text("42\nnull\n", encoding="utf-8")
        assert batch_analyze.jsonl_to_atif(session) is None


# ─── extract_text / extract_tool_calls / extract_tool_results / extract_thinking


def test_extract_text_joins_text_and_tool_result_blocks():
    content = [
        {"type": "text", "text": "one"},
        {"type": "tool_result", "content": "the output"},
    ]
    assert batch_analyze.extract_text(content) == "one\nthe output"


def test_extract_text_passes_plain_strings_through():
    assert batch_analyze.extract_text("hello") == "hello"
    assert batch_analyze.extract_text(["plain", {"type": "text", "text": "block"}]) == (
        "plain\nblock"
    )


def test_extract_text_yields_empty_for_unknown_shapes():
    assert batch_analyze.extract_text(None) == ""
    assert batch_analyze.extract_text(42) == ""
    assert batch_analyze.extract_text([{"type": "image"}]) == ""


def test_extract_tool_calls_maps_tool_use_blocks():
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
    assert batch_analyze.extract_tool_calls("text only") == []


def test_extract_tool_results_flattens_and_truncates():
    nested = [
        {
            "type": "tool_result",
            "tool_use_id": "tu-1",
            "content": [{"text": "line one"}, {"text": "line two"}],
        }
    ]
    assert batch_analyze.extract_tool_results(nested) == [
        {"source_call_id": "tu-1", "content": "line one line two"}
    ]
    long = [{"type": "tool_result", "tool_use_id": "tu-1", "content": "x" * 3000}]
    assert len(batch_analyze.extract_tool_results(long)[0]["content"]) == 2000


def test_extract_thinking_joins_blocks_and_yields_none_when_absent():
    content = [
        {"type": "thinking", "thinking": "first"},
        {"type": "thinking", "thinking": "second"},
    ]
    assert batch_analyze.extract_thinking(content) == "first\nsecond"
    assert batch_analyze.extract_thinking([{"type": "text", "text": "hi"}]) is None
    assert batch_analyze.extract_thinking("plain") is None


# ─── jsonl_to_atif ────────────────────────────────────────────────────────────


def make_session(tmp: str, name: str = "session-42") -> Path:
    session = Path(tmp) / ".claude" / "projects" / f"{name}.jsonl"
    session.parent.mkdir(parents=True)
    return session


def test_user_and_agent_steps_are_numbered_and_sourced():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
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
        doc = batch_analyze.jsonl_to_atif(session)

    assert doc["schema_version"] == "ATIF-v1.7"
    assert doc["session_id"] == "session-42"
    assert doc["agent"]["name"] == "claude-code"
    assert doc["agent"]["model_name"] == "claude-sonnet"
    assert [step["source"] for step in doc["steps"]] == ["user", "agent"]
    assert [step["step_id"] for step in doc["steps"]] == [1, 2]
    assert doc["steps"][0]["message"] == "please list files"
    assert doc["steps"][1]["message"] == "listing files"


def test_tool_results_attach_to_the_previous_agent_step():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
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
                        {
                            "type": "tool_result",
                            "tool_use_id": "tu-1",
                            "content": "file-a\nfile-b",
                        }
                    ],
                },
            },
        ])
        doc = batch_analyze.jsonl_to_atif(session)

    # the tool result did not open a third step: it attached as the
    # observation of the agent step that called the tool
    assert len(doc["steps"]) == 2
    observation = doc["steps"][1]["observation"]["results"]
    assert observation == [{"source_call_id": "tu-1", "content": "file-a\nfile-b"}]


def test_usage_metrics_map_onto_the_agent_step():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
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
        doc = batch_analyze.jsonl_to_atif(session)

    assert doc["steps"][1]["metrics"] == {
        "prompt_tokens": 10,
        "completion_tokens": 20,
        "cached_tokens": 5,
    }


def test_runtime_config_supplies_the_model():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
        write_jsonl(session, [
            {"type": "runtime-config", "model": "qoder-x"},
            {"type": "user", "message": {"role": "user", "content": "hi"}},
            {
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "yo"}]},
            },
        ])
        doc = batch_analyze.jsonl_to_atif(session)

    # the assistant step carries the runtime-config model because the
    # message itself had none
    assert doc["steps"][1]["model_name"] == "qoder-x"


def test_agent_name_comes_from_the_directory():
    with tempfile.TemporaryDirectory() as tmp:
        qoder_session = Path(tmp) / ".qoder" / "projects" / "s1.jsonl"
        qoder_session.parent.mkdir(parents=True)
        write_jsonl(qoder_session, [
            {"type": "user", "message": {"role": "user", "content": "hi"}},
        ])
        plain_session = Path(tmp) / "exports" / "s2.jsonl"
        plain_session.parent.mkdir(parents=True)
        write_jsonl(plain_session, [
            {"type": "user", "message": {"role": "user", "content": "hi"}},
        ])

        assert batch_analyze.jsonl_to_atif(qoder_session)["agent"]["name"] == "qoder"
        assert batch_analyze.jsonl_to_atif(plain_session)["agent"]["name"] == "unknown"


def test_noise_entries_are_skipped_and_bad_lines_tolerated():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
        session.write_text(
            "\n".join([
                json.dumps({"type": "mode", "mode": "acceptEdits"}),
                json.dumps({"type": "summary", "summary": "old chat"}),
                "{ this line is not json",
                json.dumps({"type": "user", "message": {"role": "user", "content": "real"}}),
            ]),
            encoding="utf-8",
        )
        doc = batch_analyze.jsonl_to_atif(session)

    assert doc is not None
    assert len(doc["steps"]) == 1
    assert doc["steps"][0]["message"] == "real"


def test_long_messages_are_truncated_to_five_thousand_chars():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
        write_jsonl(session, [
            {"type": "user", "message": {"role": "user", "content": "x" * 6000}},
        ])
        doc = batch_analyze.jsonl_to_atif(session)

    assert len(doc["steps"][0]["message"]) == 5000


def test_stepless_files_return_none_beyond_the_scalar_case():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_session(tmp)
        write_jsonl(session, [{"type": "mode", "mode": "default"}])
        assert batch_analyze.jsonl_to_atif(session) is None


# ─── find_conversation_files ──────────────────────────────────────────────────


def test_find_conversation_files_filters_and_orders_by_recency():
    with tempfile.TemporaryDirectory() as tmp:
        projects = Path(tmp) / "projects"
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


def test_find_conversation_files_tolerates_missing_directories():
    with tempfile.TemporaryDirectory() as tmp:
        assert batch_analyze.find_conversation_files([Path(tmp) / "nope"]) == []


# ─── main ─────────────────────────────────────────────────────────────────────


def make_main_session(tmp: str, steps: int) -> Path:
    session = Path(tmp) / ".claude" / "projects" / "batch-session.jsonl"
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


def run_main(argv: list, run_result=None, run_error=None):
    import io

    captured = {}

    def fake_run(cmd, **kwargs):
        captured["cmd"] = cmd
        captured["kwargs"] = kwargs
        if run_error is not None:
            raise run_error
        return run_result

    original_run = batch_analyze.subprocess.run
    original_argv = sys.argv
    original_out, original_err = sys.stdout, sys.stderr
    batch_analyze.subprocess.run = fake_run
    sys.argv = ["batch-analyze.py"] + argv
    sys.stdout = io.StringIO()
    sys.stderr = io.StringIO()
    try:
        batch_analyze.main()
    finally:
        captured["stdout"] = sys.stdout.getvalue()
        captured["stderr"] = sys.stderr.getvalue()
        batch_analyze.subprocess.run = original_run
        sys.argv = original_argv
        sys.stdout, sys.stderr = original_out, original_err
    return captured


def test_main_analyzes_a_single_file_and_attributes_the_output():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_main_session(tmp, steps=3)
        analyze_output = {"dimension": "perf", "score": 0.9}

        captured = run_main(
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(analyze_output), stderr=""
            ),
        )

    # the analyze binary receives the converted ATIF temp file and the dim
    assert captured["cmd"][0] == "/fake/analyze"
    assert captured["cmd"][2:] == ["--dim", "perf"]
    assert Path(captured["cmd"][1]).exists() is False, "temp ATIF file must be cleaned up"
    report = json.loads(captured["stdout"])
    assert report["_source_file"] == str(session)
    assert report["_session_id"] == "batch-session"
    assert report["_steps"] == 4
    assert report["score"] == 0.9


def test_main_skips_conversations_below_the_step_floor():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_main_session(tmp, steps=1)

        captured = run_main(
            ["--dim", "cost", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(args=[], returncode=0, stdout="{}", stderr=""),
        )

    assert "SKIP" in captured["stderr"]
    assert "0 analyzed" in captured["stderr"]


def test_main_counts_analyze_failures_instead_of_raising():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_main_session(tmp, steps=3)

        captured = run_main(
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(
                args=[], returncode=1, stdout="", stderr="boom"
            ),
        )

    assert "1 failed" in captured["stderr"]


def test_main_counts_invalid_analyze_output_instead_of_raising():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_main_session(tmp, steps=3)

        captured = run_main(
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_result=subprocess.CompletedProcess(
                args=[], returncode=0, stdout="not-json", stderr=""
            ),
        )

    assert "1 failed" in captured["stderr"]


def test_main_counts_analyze_timeouts_instead_of_raising():
    with tempfile.TemporaryDirectory() as tmp:
        session = make_main_session(tmp, steps=3)

        captured = run_main(
            ["--dim", "perf", "--file", str(session), "--binary", "/fake/analyze"],
            run_error=subprocess.TimeoutExpired(cmd=[], timeout=300),
        )

    assert "TIMEOUT" in captured["stderr"]
