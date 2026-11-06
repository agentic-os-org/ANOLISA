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

"""Test batch result aggregation and statistics.

Covers:
- pass_at_k calculation
- batch_results.json structure
- batch_summary.json structure
- Multi-trial aggregation
- Error handling

Task type coverage:
- T tasks: single/multi trial
- M tasks: sandbox flag + env_snapshot
- C tasks: user_agent_rounds
"""

import ast
import importlib.util
import math
from pathlib import Path
from typing import Any

import pytest


def _aggregate_completed_trials(entries: list[dict[str, Any]]) -> dict[str, Any]:
    # Execute only the aggregation section of the real run_batch, isolating the
    # setup steps that would start the gateway, Docker and an LLM.
    source = Path(__file__).parents[1] / "src" / "ce_runner" / "batch_runner.py"
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
    return context


def _completed_trial(task_id: str, number: int) -> dict[str, Any]:
    score = 0.2 if number == 1 else 0.9
    return {
        "task_id": task_id,
        "trial": {
            "trial": number,
            "task_score": score,
            "passed": score >= 0.75,
            "wall_time_s": 1.0,
        },
    }


@pytest.mark.parametrize("completion_order", [[2, 1], [3, 2, 1], [1, 2], [1]])
def test_trial_identity_survives_grading_completion_order(completion_order: list[int]) -> None:
    entries = [_completed_trial("task-a", number) for number in completion_order]
    context = _aggregate_completed_trials(entries)
    task = context["task_results"]["task-a"]
    assert [trial["trial"] for trial in task["trials"]] == sorted(completion_order)
    assert {trial["trial"]: trial["task_score"] for trial in task["trials"]} == {
        number: (0.2 if number == 1 else 0.9) for number in completion_order
    }
    assert task["avg_score"] == pytest.approx(
        sum(entry["trial"]["task_score"] for entry in entries) / len(entries), abs=0.0001
    )
    assert context["total_wall_time"] == len(entries)


def test_interleaved_tasks_keep_trial_labels_in_the_real_report() -> None:
    context = _aggregate_completed_trials(
        [
            _completed_trial("task-b", 2),
            _completed_trial("task-a", 2),
            _completed_trial("task-b", 1),
            _completed_trial("task-a", 1),
        ]
    )
    data = [
        dict(task, task_name="fixture", difficulty="easy")
        for task in context["task_results"].values()
    ]
    path = Path(__file__).parents[1] / "scripts" / "summarize_results.py"
    spec = importlib.util.spec_from_file_location("fixture_summary", path)
    assert spec is not None and spec.loader is not None
    summary = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(summary)
    rows = summary.build_table(data)
    label_index = rows[0].index("Trial")
    score_index = rows[0].index("Task Score")
    assert [(row[label_index], row[score_index]) for row in rows[1:]] == [
        ("#1", "0.20"),
        ("#2", "0.90"),
        ("#1", "0.20"),
        ("#2", "0.90"),
    ]
    assert context["n_pass_at_1"] == 2
    assert context["n_pass_hat_1"] == 0


class TestPassAtK:
    """Test pass@k estimator."""

    def test_pass_at_k_all_pass(self):
        """All trials pass -> pass@k = 1.0."""
        from ce_runner.batch_runner import pass_at_k

        assert pass_at_k(5, 5, 1) == 1.0

    def test_pass_at_k_none_pass(self):
        """No trials pass -> pass@k = 0.0."""
        from ce_runner.batch_runner import pass_at_k

        assert pass_at_k(5, 0, 1) == 0.0

    def test_pass_at_k_partial(self):
        """Partial pass rate."""
        from ce_runner.batch_runner import pass_at_k

        result = pass_at_k(10, 5, 1)
        assert 0.0 < result < 1.0

    def test_pass_at_k_k_greater_than_n_minus_c(self):
        """k > n-c returns 1.0."""
        from ce_runner.batch_runner import pass_at_k

        # n=5, c=4, k=2 -> n-c=1 < k -> returns 1.0
        assert pass_at_k(5, 4, 2) == 1.0
