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

"""Verify opt-in deterministic trial reports without a second model call."""

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "generate_trial_reports.py"


@pytest.fixture
def reporter() -> ModuleType:
    spec = importlib.util.spec_from_file_location("offline_reporter", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _inputs(tmp_path: Path) -> tuple[Path, Path]:
    tasks = tmp_path / "tasks"
    task = tasks / "T001_example"
    task.mkdir(parents=True)
    (task / "task.yaml").write_text(
        "task_id: T001_example\ntask_name: 测试任务\n"
        "primary_dimensions: [correctness]\njudge_rubric: 计算准确\n",
        encoding="utf-8",
    )
    traces = tmp_path / "traces"
    traces.mkdir()
    events = {
        "passed": [{"type": "grading_result", "passed": True, "task_score": 1}],
        "failed": [
            {
                "type": "grading_result",
                "passed": False,
                "task_score": 0.25,
                "scores": {"correctness": 0.25, "communication": 0.1},
                "judge_calls": [{"score": 0.25, "reasoning": "答案不正确"}],
            },
            {"type": "trace_end", "total_turns": 3, "wall_time_s": 5},
        ],
        "missing": [{"type": "trace_end", "total_turns": 2}],
    }
    for trial, content in events.items():
        (traces / f"T001_example_{trial}.jsonl").write_text(
            "\n".join(json.dumps(event, ensure_ascii=False) for event in content),
            encoding="utf-8",
        )
    return tasks, traces


def test_offline_cli_works_without_sdk_or_credentials(tmp_path: Path) -> None:
    tasks, traces = _inputs(tmp_path)
    blocker = tmp_path / "no_sdk"
    blocker.mkdir()
    (blocker / "sitecustomize.py").write_text(
        "import builtins\n"
        "original_import = builtins.__import__\n"
        "def guarded_import(name, *args, **kwargs):\n"
        "    if name == 'openai' or name.startswith('openai.'):\n"
        "        raise ImportError('OpenAI SDK intentionally unavailable')\n"
        "    return original_import(name, *args, **kwargs)\n"
        "builtins.__import__ = guarded_import\n",
        encoding="utf-8",
    )
    output = tmp_path / "reports"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--no-llm",
            "--trace-dir",
            str(traces),
            "--tasks-dir",
            str(tasks),
            "--output-dir",
            str(output),
        ],
        cwd=tmp_path,
        env={**os.environ, "PYTHONPATH": str(blocker)},
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    assert result.returncode == 0, result.stderr
    reports = {
        p.stem.rsplit("_", 1)[-1]: json.loads(p.read_text(encoding="utf-8"))
        for p in output.glob("*.json")
    }
    assert {name: report["status"] for name, report in reports.items()} == {
        "passed": "succ",
        "failed": "fail",
        "missing": "error",
    }
    failed = reports["failed"]
    assert failed["task_name"] == "测试任务"
    assert failed["task_score"] == 0.25
    assert failed["scores"] == {"correctness": 0.25, "communication": 0.1}
    assert failed["failure_reason"][0]["reasoning"] == "答案不正确"
    assert failed["failure_reason"][1]["low_dimension_scores"] == {"correctness": 0.25}
    assert failed["task_judge_rubric"] == "计算准确"
    assert (failed["total_turns"], failed["wall_time_s"]) == (3, 5)
    assert all("failure_classification" not in report for report in reports.values())
    assert "Done: 1 succ, 1 fail, 1 error" in result.stdout
    assert "LLM classification: disabled (--no-llm)" in result.stdout


def test_offline_config_disables_only_classification(
    reporter: ModuleType, tmp_path: Path
) -> None:
    config = tmp_path / "config.yaml"
    config.write_text("judge:\n  model_id: unused\n", encoding="utf-8")
    args = SimpleNamespace(
        trace_dir=None,
        tasks_dir=None,
        output_dir=None,
        judge_model=None,
        judge_base_url=None,
        judge_api_key=None,
        no_llm=True,
    )
    settings = reporter.load_config(str(config), args)
    assert settings["classify_failures"] is False
    assert settings["trace_dir"] == str(tmp_path / "traces")
    assert settings["judge_model_id"] == "unused"


@pytest.mark.parametrize("classify", [None, True, False])
def test_process_keeps_default_classification(
    reporter: ModuleType,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    classify: bool | None,
) -> None:
    tasks, traces = _inputs(tmp_path)
    calls = []
    monkeypatch.setattr(
        reporter,
        "llm_classify_failure",
        lambda *args: calls.append(args) or {"category": "incorrect_execution"},
    )
    settings = {
        "tasks_dir": str(tasks),
        "judge_api_key": "test-key",
        "judge_base_url": "http://example.invalid",
        "judge_model_id": "test-model",
    }
    if classify is not None:
        settings["classify_failures"] = classify
    _, report = reporter.process_one_trace(
        str(traces / "T001_example_failed.jsonl"), settings
    )
    assert len(calls) == (0 if classify is False else 1)
    assert ("failure_classification" in report) is (classify is not False)


def test_default_main_classifies_only_failed_trials(
    reporter: ModuleType,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    tasks, traces = _inputs(tmp_path)
    output = tmp_path / "reports"
    calls = []
    classification = {"category": "incorrect_execution", "key_reason_zh": "错误"}
    monkeypatch.setattr(
        reporter,
        "llm_classify_failure",
        lambda *args: calls.append(args) or classification,
    )
    monkeypatch.setattr(
        sys,
        "argv",
        [
            str(SCRIPT),
            "--trace-dir",
            str(traces),
            "--tasks-dir",
            str(tasks),
            "--output-dir",
            str(output),
            "--judge-api-key",
            "test-key",
            "--judge-model",
            "test-model",
        ],
    )
    reporter.main()
    assert len(calls) == 1
    failed = json.loads(
        (output / "T001_example_failed.json").read_text(encoding="utf-8")
    )
    assert failed["failure_classification"] == classification
    assert "LLM classification: 1 ok, 0 errors" in capsys.readouterr().out


@pytest.mark.parametrize(
    "extra, message", [([], "api_key"), (["--judge-api-key", "test"], "model_id")]
)
def test_default_cli_still_requires_judge_config(
    tmp_path: Path, extra: list[str], message: str
) -> None:
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--output-dir",
            str(tmp_path / "reports"),
            *extra,
        ],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    assert result.returncode == 1
    assert message in result.stderr
    assert not (tmp_path / "reports").exists()
