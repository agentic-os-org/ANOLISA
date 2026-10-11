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

"""Exercise task selection through the actual report-generation CLI."""

import importlib.util
import json
import sys
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


@pytest.fixture
def invocation(tmp_path, monkeypatch):
    monkeypatch.syspath_prepend(str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(
        "selection_reporter", SCRIPTS / "generate_trial_reports.py"
    )
    reporter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reporter)
    traces = tmp_path / "traces"
    traces.mkdir()
    names = [
        "alpha_case_002.jsonl",
        "beta_case_003.jsonl",
        "alpha_case_001.jsonl",
        "gamma_004.jsonl",
    ]
    for name in names:
        (traces / name).write_text("fixture evidence", encoding="utf-8")
    (traces / "alpha_case_ignored.json").write_text("ignored", encoding="utf-8")
    output = tmp_path / "reports"
    calls = []

    def process(trace_path, settings):
        filename = Path(trace_path).name
        calls.append(filename)
        status = "fail" if filename.startswith("beta") else "succ"
        return filename, {
            "status": status,
            "task_id": reporter.resolve_task_id(filename),
            "trace_file": filename,
        }

    monkeypatch.setattr(reporter, "process_one_trace", process)

    def run(*selectors):
        args = [
            "generate_trial_reports.py",
            "--trace-dir",
            str(traces),
            "--output-dir",
            str(output),
            "--judge-api-key",
            "fixture-key",
            "--judge-model",
            "fixture-model",
            *selectors,
        ]
        monkeypatch.setattr(sys, "argv", args)
        reporter.main()

    return run, output, calls, traces


def test_default_reports_all_trials(invocation, capsys):
    run, output, calls, _ = invocation
    run()
    assert calls == [
        "alpha_case_001.jsonl",
        "alpha_case_002.jsonl",
        "beta_case_003.jsonl",
        "gamma_004.jsonl",
    ]
    assert len(list(output.glob("*.json"))) == 4
    assert "Done: 3 succ, 1 fail, 0 error" in capsys.readouterr().out


def test_select_one_task_retains_all_trials(invocation, capsys):
    run, output, calls, traces = invocation
    originals = {path.name: path.read_bytes() for path in traces.iterdir()}
    run("--task-id", "alpha_case")
    assert calls == ["alpha_case_001.jsonl", "alpha_case_002.jsonl"]
    assert sorted(path.name for path in output.glob("*.json")) == [
        "alpha_case_001.json",
        "alpha_case_002.json",
    ]
    assert {path.name: path.read_bytes() for path in traces.iterdir()} == originals
    assert "Found 2 trace files" in capsys.readouterr().out
    report = json.loads((output / "alpha_case_001.json").read_text())
    assert report["task_id"] == "alpha_case"


def test_multiple_and_repeated_ids_keep_filename_order(invocation, capsys):
    run, output, calls, _ = invocation
    run("--task-id", "gamma", "--task-id", "beta_case", "--task-id", "gamma")
    assert calls == ["beta_case_003.jsonl", "gamma_004.jsonl"]
    assert len(list(output.glob("*.json"))) == 2
    assert "Done: 1 succ, 1 fail, 0 error" in capsys.readouterr().out


@pytest.mark.parametrize("task_id", ["alpha", "unknown", ""])
def test_invalid_selection_stops_before_output_or_classification(
    invocation, capsys, task_id
):
    run, output, calls, _ = invocation
    with pytest.raises(SystemExit) as error:
        run("--task-id", "alpha_case", "--task-id", task_id)
    assert error.value.code == 2
    assert calls == []
    assert not output.exists()
    message = capsys.readouterr().err
    assert (
        "cannot be empty" if not task_id else "No trace files found for task ID(s)"
    ) in message


def test_empty_selection_value_is_a_usage_error(invocation, capsys):
    run, output, calls, _ = invocation
    with pytest.raises(SystemExit) as error:
        run("--task-id", "   ")
    assert error.value.code == 2
    assert "cannot be empty" in capsys.readouterr().err
    assert not output.exists()
    assert calls == []


def test_task_id_uses_existing_filename_convention(invocation):
    run, output, calls, traces = invocation
    (traces / "task_with_many_parts_trial5.jsonl").write_text(
        "fixture", encoding="utf-8"
    )
    run("--task-id", "task_with_many_parts")
    assert calls == ["task_with_many_parts_trial5.jsonl"]
    assert (output / "task_with_many_parts_trial5.json").exists()


def test_invalid_selection_preserves_existing_report_files(invocation):
    run, output, calls, _ = invocation
    output.mkdir()
    sentinel = output / "alpha_case_001.json"
    sentinel.write_text("existing report", encoding="utf-8")
    with pytest.raises(SystemExit):
        run("--task-id", "missing")
    assert sentinel.read_text() == "existing report"
    assert calls == []


def test_surrounding_selector_whitespace_is_ignored(invocation):
    run, _, calls, _ = invocation
    run("--task-id", " alpha_case ")
    assert calls == ["alpha_case_001.jsonl", "alpha_case_002.jsonl"]


def test_only_selected_failed_trials_reach_classifier(tmp_path, monkeypatch):
    monkeypatch.syspath_prepend(str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(
        "real_selection_reporter", SCRIPTS / "generate_trial_reports.py"
    )
    reporter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reporter)
    traces = tmp_path / "traces"
    tasks = tmp_path / "tasks"
    output = tmp_path / "reports"
    traces.mkdir()
    events = [
        {
            "type": "grading_result",
            "passed": False,
            "task_score": 0.2,
            "scores": {"accuracy": 0.2},
        },
        {"type": "trace_end", "total_turns": 3, "wall_time_s": 4},
    ]
    for task_id in ["alpha_case", "beta_case"]:
        task_dir = tasks / task_id
        task_dir.mkdir(parents=True)
        (task_dir / "task.yaml").write_text(
            "task_name: Review evidence\nprimary_dimensions: [accuracy]\n",
            encoding="utf-8",
        )
        (traces / f"{task_id}_trial1.jsonl").write_text(
            "".join(json.dumps(event) + "\n" for event in events), encoding="utf-8"
        )
    classified = []

    def classify(task_info, *_args):
        classified.append(task_info["task_id"])
        return {"category": "tool_error", "key_reason_zh": "fixture classification"}

    monkeypatch.setattr(reporter, "llm_classify_failure", classify)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "generate_trial_reports.py",
            "--trace-dir",
            str(traces),
            "--tasks-dir",
            str(tasks),
            "--output-dir",
            str(output),
            "--judge-api-key",
            "fixture-key",
            "--judge-model",
            "fixture-model",
            "--task-id",
            "alpha_case",
        ],
    )
    reporter.main()
    assert classified == ["alpha_case"]
    assert sorted(path.name for path in output.iterdir()) == ["alpha_case_trial1.json"]
    report = json.loads((output / "alpha_case_trial1.json").read_text())
    assert report["status"] == "fail"
    assert report["total_turns"] == 3
    assert report["failure_classification"]["category"] == "tool_error"
