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

"""Check the classification counters with synthetic traces and an LLM stub.

No model service is contacted.
"""

import importlib.util
import json
import sys
import types
from pathlib import Path
from typing import Any

import pytest


@pytest.fixture
def reports(monkeypatch: pytest.MonkeyPatch) -> Any:
    openai = types.ModuleType("openai")
    openai.OpenAI = object
    monkeypatch.setitem(sys.modules, "openai", openai)
    source = Path(__file__).parents[1] / "scripts" / "generate_trial_reports.py"
    spec = importlib.util.spec_from_file_location("fixture_trial_reports", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _run_report(
    reports: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    passed: bool,
    answer: str | Exception,
) -> dict[str, Any]:
    traces = tmp_path / "traces"
    traces.mkdir()
    task = tmp_path / "tasks" / "fixture-task"
    task.mkdir(parents=True)
    (task / "task.yaml").write_text("task_id: fixture-task\n", encoding="utf-8")
    trace = traces / "fixture-task_trial.jsonl"
    trace.write_text(
        json.dumps(
            {
                "type": "grading_result",
                "scores": {"completion": 0.0},
                "task_score": 1.0 if passed else 0.0,
                "passed": passed,
            }
        )
        + "\n",
        encoding="utf-8",
    )
    output = tmp_path / "reports"

    def create(**kwargs: Any) -> Any:
        assert not passed, "passing trials must not request a failure classification"
        if isinstance(answer, Exception):
            raise answer
        return types.SimpleNamespace(
            choices=[types.SimpleNamespace(message=types.SimpleNamespace(content=answer))]
        )

    client = types.SimpleNamespace(
        chat=types.SimpleNamespace(completions=types.SimpleNamespace(create=create))
    )
    monkeypatch.setattr(reports, "get_llm_client", lambda *args: client)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "generate_trial_reports",
            "--trace-dir",
            str(traces),
            "--tasks-dir",
            str(task.parent),
            "--output-dir",
            str(output),
            "--judge-model",
            "fixture-judge",
            "--judge-api-key",
            "fixture-only",
        ],
    )
    reports.main()
    return json.loads((output / "fixture-task_trial.json").read_text(encoding="utf-8"))


@pytest.mark.parametrize("answer", [RuntimeError("fixture service unavailable"), "not JSON"])
def test_failed_llm_classification_is_counted_as_an_error(
    reports: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    answer: str | Exception,
) -> None:
    report = _run_report(reports, monkeypatch, tmp_path, False, answer)
    assert report["failure_classification"]["key_reason_zh"].startswith("LLM error:")
    assert "LLM classification: 0 ok, 1 errors" in capsys.readouterr().out
    assert report["status"] == "fail"
    assert report["task_score"] == 0.0


@pytest.mark.parametrize("category", ["other", "tool_error"])
def test_successful_classifications_including_other_are_counted_as_ok(
    reports: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    category: str,
) -> None:
    classification = {"category": category, "key_reason_zh": "fixture explanation"}
    report = _run_report(reports, monkeypatch, tmp_path, False, json.dumps(classification))
    assert report["failure_classification"] == classification
    assert "LLM classification: 1 ok, 0 errors" in capsys.readouterr().out


def test_passing_trial_does_not_increment_llm_health_counts(
    reports: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    report = _run_report(reports, monkeypatch, tmp_path, True, RuntimeError("must not call"))
    assert report["status"] == "succ"
    assert "failure_classification" not in report
    assert "LLM classification: 0 ok, 0 errors" in capsys.readouterr().out
