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

"""Isolate the external grader to verify the failure marker and the score
denominator of the real batch aggregation.
"""

import ast
import importlib.util
import json
import math
import sys
import types
from pathlib import Path
from typing import Any

import pytest

RUNNER = Path(__file__).parents[1] / "src" / "ce_runner"


@pytest.fixture
def pipeline(monkeypatch: pytest.MonkeyPatch) -> Any:
    # Substitute only the module initialisation dependencies; the complete
    # phase_grade still executes from the real source file.
    package = types.ModuleType("ce_runner")
    package.__path__ = [str(RUNNER)]
    common = types.ModuleType("ce_runner._common")
    common._PYTHON = sys.executable
    common.log = lambda *args: None
    monkeypatch.setitem(sys.modules, "ce_runner", package)
    monkeypatch.setitem(sys.modules, "ce_runner._common", common)
    spec = importlib.util.spec_from_file_location("ce_runner.pipeline", RUNNER / "pipeline.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _grade(
    pipeline: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    returncode: int,
    score: float | None = None,
) -> dict[str, Any]:
    trace = tmp_path / "fixture.jsonl"
    trace.write_text('{"type":"trace_start"}\n', encoding="utf-8")

    def run(cmd: list[str], **kwargs: Any) -> Any:
        if score is not None:
            with trace.open("a", encoding="utf-8") as stream:
                stream.write(
                    json.dumps(
                        {
                            "type": "grading_result",
                            "scores": {
                                "completion": score,
                                "robustness": score,
                                "communication": score,
                                "safety": 1.0,
                            },
                            "task_score": score,
                            "passed": score >= 0.75,
                        }
                    )
                    + "\n"
                )
        if returncode:
            kwargs["stderr"].write(b"fixture grader failure\n")
        return types.SimpleNamespace(returncode=returncode)

    monkeypatch.setattr(pipeline.subprocess, "run", run)
    return pipeline.phase_grade(
        str(trace),
        "fixture.yaml",
        {
            "model": "fixture-judge",
            "base_url": "",
            "api_key": "fixture-only",
        },
    )


@pytest.mark.parametrize("returncode", [1, -9])
def test_grader_crash_without_a_result_is_an_error(
    pipeline: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    returncode: int,
) -> None:
    result = _grade(pipeline, monkeypatch, tmp_path, returncode)
    assert result.get("error") == f"grader_rc_{returncode}"
    assert result["passed"] is False
    assert (tmp_path / "grader_fixture.err.log").read_bytes() == b"fixture grader failure\n"


@pytest.mark.parametrize("score", [0.0, 0.9])
def test_successfully_graded_zero_and_positive_scores_remain_authoritative(
    pipeline: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    score: float,
) -> None:
    result = _grade(pipeline, monkeypatch, tmp_path, 0, score)
    assert not result.get("error")
    assert result["task_score"] == score
    assert result["passed"] is (score >= 0.75)


def test_result_written_before_teardown_failure_remains_authoritative(
    pipeline: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    result = _grade(pipeline, monkeypatch, tmp_path, 1, 0.9)
    assert not result.get("error")
    assert result["task_score"] == 0.9
    assert result["passed"] is True


def test_grader_crash_is_excluded_from_the_real_batch_average(
    pipeline: Any,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    result = _grade(pipeline, monkeypatch, tmp_path, 1)
    failed = dict(result, trial=1, wall_time_s=1.0)
    healthy = dict(failed, trial=2, task_score=0.9, passed=True, error=None)
    entries = [{"task_id": "fixture-task", "trial": trial} for trial in (failed, healthy)]
    source = RUNNER / "batch_runner.py"
    tree = ast.parse(source.read_text(encoding="utf-8"))
    run = next(
        node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "run_batch"
    )

    def assigns(node: ast.stmt, name: str) -> bool:
        return isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == name for target in node.targets
        )

    begin = next(i for i, node in enumerate(run.body) if assigns(node, "task_results"))
    end = next(i for i, node in enumerate(run.body) if assigns(node, "avg_score_final"))
    estimator = next(
        node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "pass_at_k"
    )
    context = {"math": math, "batch_results": entries}
    module = ast.Module(body=[estimator, *run.body[begin : end + 1]], type_ignores=[])
    exec(compile(module, str(source), "exec"), context)
    assert context["task_results"]["fixture-task"]["avg_score"] == 0.9
    assert context["avg_score_final"] == 0.9
    assert context["n_errored"] == 1
