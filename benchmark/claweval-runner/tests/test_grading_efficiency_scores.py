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

"""trace_end keeps the converter's efficiency scores after grading.

The converter derives `efficiency_turns/tokens/wall_time_s` from the session.
The grader never receives those totals, so its `Scores` copy carries the
dataclass defaults (zeros); merging it over trace_end.scores clobbered the real
values on every grading run.
"""

import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any

import pytest

CONVERTER_EFFICIENCY = {
    "efficiency_turns": 7,
    "efficiency_tokens": 5300,
    "efficiency_wall_time_s": 412.5,
}


@pytest.fixture
def grader_module(monkeypatch: pytest.MonkeyPatch) -> ModuleType:
    module_names = [
        "claw_eval",
        "claw_eval.graders",
        "claw_eval.graders.registry",
        "claw_eval.graders.llm_judge",
        "claw_eval.models",
        "claw_eval.models.scoring",
        "claw_eval.models.task",
        "claw_eval.trace",
        "claw_eval.trace.reader",
    ]
    modules = {name: ModuleType(name) for name in module_names}
    for name, module in modules.items():
        monkeypatch.setitem(sys.modules, name, module)

    scores = SimpleNamespace(
        completion=1.0,
        robustness=1.0,
        communication=1.0,
        safety=1.0,
        efficiency_turns=0.0,
        efficiency_tokens=0.0,
        efficiency_wall_time_s=0.0,
    )
    modules["claw_eval.graders.registry"].get_grader = lambda *a, **k: SimpleNamespace(
        grade=lambda *a, **k: scores
    )
    modules["claw_eval.graders.llm_judge"].LLMJudge = object
    modules["claw_eval.models.scoring"].compute_task_score = lambda value: 1.0
    modules["claw_eval.models.scoring"].is_pass = lambda value: True
    modules["claw_eval.models.task"].TaskDefinition = SimpleNamespace(
        from_yaml=lambda path: SimpleNamespace(task_id="T001")
    )
    modules["claw_eval.trace.reader"].load_trace = lambda path: (
        SimpleNamespace(trace_id="trace-1", task_id="T001"),
        [],
        [],
        [],
        {},
        {},
    )

    source = Path(__file__).parents[1] / "src" / "ce_runner" / "grader_runner.py"
    spec = importlib.util.spec_from_file_location("efficiency_grader_under_test", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def trace(tmp_path: Path) -> Path:
    path = tmp_path / "trace.jsonl"
    events: list[dict[str, Any]] = [
        {"type": "trace_start", "trace_id": "trace-1", "task_id": "T001"},
        {
            "type": "message",
            "message": {"role": "assistant", "content": "evidence"},
        },
        {
            "type": "trace_end",
            "scores": {
                "completion": 0.0,
                "robustness": 0.0,
                "communication": 0.0,
                "safety": 0.0,
                **CONVERTER_EFFICIENCY,
            },
            "task_score": 0.0,
            "passed": False,
        },
    ]
    path.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    return path


def _trace_end(path: Path) -> dict[str, Any]:
    events = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    return next(event for event in events if event["type"] == "trace_end")


def test_grading_keeps_converter_efficiency_scores(grader_module: ModuleType, trace: Path) -> None:
    grader_module.grade_trace(str(trace), "task.yaml")

    scores = _trace_end(trace)["scores"]

    assert scores["completion"] == 1.0
    assert scores["robustness"] == 1.0
    for key, value in CONVERTER_EFFICIENCY.items():
        assert scores[key] == value


def test_grading_result_keeps_the_grader_raw_output(grader_module: ModuleType, trace: Path) -> None:
    grader_module.grade_trace(str(trace), "task.yaml")

    events = [json.loads(line) for line in trace.read_text().splitlines() if line.strip()]
    grading_result = next(event for event in events if event["type"] == "grading_result")

    assert grading_result["scores"]["efficiency_turns"] == 0.0
