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

"""Characterize the evidence consumed by both standalone report entry points."""

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


@pytest.fixture(params=["analyze", "generate_trial_reports"])
def reporter(request, monkeypatch):
    monkeypatch.syspath_prepend(str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(
        f"report_fixture_{request.param}", SCRIPTS / f"{request.param}.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_task_projection_and_truncation(reporter, tmp_path):
    task_dir = tmp_path / "folder-id"
    task_dir.mkdir()
    metadata = {
        "task_id": "declared-id",
        "task_name": "Invoice review",
        "category": "finance",
        "difficulty": "hard",
        "prompt": {"text": "\u8bc1\u636e" * 180, "unused": "private"},
        "scoring_components": [{"name": "accuracy", "weight": 1}],
        "judge_rubric": "Use the invoice evidence",
        "reference_solution": "s" * 320,
        "primary_dimensions": ["accuracy"],
        "unused": "not projected",
    }
    (task_dir / "task.yaml").write_text(yaml.safe_dump(metadata), encoding="utf-8")
    expected = {
        key: value for key, value in metadata.items() if key not in ("prompt", "unused")
    }
    expected["prompt"] = metadata["prompt"]["text"][:300]
    expected["reference_solution"] = "s" * 300
    assert reporter.load_task_info("folder-id", str(tmp_path)) == expected


def test_missing_task_fallback(reporter, tmp_path):
    assert reporter.load_task_info("missing", str(tmp_path)) == {
        "task_id": "missing",
        "error": "task.yaml not found",
    }


def test_defaults_use_requested_task_id(reporter, tmp_path):
    task_dir = tmp_path / "task"
    task_dir.mkdir()
    (task_dir / "task.yaml").write_text("task_name: title\n", encoding="utf-8")
    assert reporter.load_task_info("task", str(tmp_path)) == {
        "task_id": "task",
        "task_name": "title",
        "category": "",
        "difficulty": "",
        "prompt": "",
        "scoring_components": [],
        "judge_rubric": "",
        "reference_solution": "",
        "primary_dimensions": [],
    }


def test_latest_grading_and_trace_end_are_independent(reporter, tmp_path):
    grading = {"type": "grading_result", "passed": True, "scores": {"accuracy": 1}}
    trace_end = {"type": "trace_end", "total_turns": 12, "wall_time_s": 4.2}
    events = [
        {"type": "grading_result", "passed": False},
        {"type": "trace_end", "total_turns": 1},
        grading,
        {"type": "conversation", "messages": []},
        trace_end,
    ]
    trace = tmp_path / "trace.jsonl"
    trace.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    assert reporter.load_grading_result(str(trace)) == (grading, trace_end)


@pytest.mark.parametrize(
    "events,expected",
    [
        ([], (None, None)),
        ([{"type": "trace_start"}], (None, None)),
        (
            [{"type": "grading_result", "passed": False}],
            ({"type": "grading_result", "passed": False}, None),
        ),
        (
            [{"type": "trace_end", "total_turns": 0}],
            (None, {"type": "trace_end", "total_turns": 0}),
        ),
    ],
)
def test_absent_events(reporter, tmp_path, events, expected):
    trace = tmp_path / "trace.jsonl"
    trace.write_text(
        "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
    )
    assert reporter.load_grading_result(str(trace)) == expected


@pytest.mark.parametrize("content", ["not JSON\n", "\n"])
def test_invalid_trace_propagates_json_error(reporter, tmp_path, content):
    trace = tmp_path / "trace.jsonl"
    trace.write_text(content, encoding="utf-8")
    with pytest.raises(json.JSONDecodeError):
        reporter.load_grading_result(str(trace))


def test_missing_trace_propagates_file_error(reporter, tmp_path):
    with pytest.raises(FileNotFoundError):
        reporter.load_grading_result(str(tmp_path / "absent.jsonl"))


def test_invalid_yaml_propagates_parse_error(reporter, tmp_path):
    task_dir = tmp_path / "task"
    task_dir.mkdir()
    (task_dir / "task.yaml").write_text("task_name: [\n", encoding="utf-8")
    with pytest.raises(yaml.YAMLError):
        reporter.load_task_info("task", str(tmp_path))


def test_caller_specific_classification_policy_is_retained(reporter):
    grading = {"task_score": 0.1, "judge_calls": []}
    scores = {"accuracy": 0.1, "presentation": 0.2, "efficiency_tokens": 0}
    if reporter.__name__.endswith("generate_trial_reports"):
        reasons = reporter.infer_failure_reason(grading, scores, ["accuracy"])
        assert {"low_dimension_scores": {"accuracy": 0.1}} in reasons
        assert any(
            reason.get("ignored_dimensions") == {"presentation": 0.2}
            for reason in reasons
        )
    else:
        assert {
            "low_dimension_scores": {"accuracy": 0.1, "presentation": 0.2}
        } in reporter.infer_failure_reason(grading, scores)


@pytest.mark.parametrize("entry", ["analyze.py", "generate_trial_reports.py"])
def test_standalone_help_imports(entry):
    result = subprocess.run(
        [sys.executable, str(SCRIPTS / entry), "--help"],
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stderr
    assert "--trace-dir" in result.stdout
