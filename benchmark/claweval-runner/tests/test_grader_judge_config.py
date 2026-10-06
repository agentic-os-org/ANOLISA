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

"""Tests for grader_runner's judge configuration assembly.

grader_runner is a standalone entry point whose module-level ``claw_eval``
import fails wherever the claw-eval checkout is absent, so the dependency is
stubbed here; the judge-config handling under test does not depend on it.
"""

from __future__ import annotations

import sys
import types
from pathlib import Path
from unittest.mock import patch

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

_STUB_NAMES = (
    "claw_eval",
    "claw_eval.graders",
    "claw_eval.graders.registry",
    "claw_eval.graders.llm_judge",
    "claw_eval.models",
    "claw_eval.models.scoring",
    "claw_eval.models.task",
    "claw_eval.trace",
    "claw_eval.trace.reader",
)


@pytest.fixture(scope="module")
def grader_mod():
    saved = {n: sys.modules.get(n) for n in _STUB_NAMES}
    for name in _STUB_NAMES:
        sys.modules[name] = types.ModuleType(name)
    sys.modules["claw_eval.graders.registry"].get_grader = lambda *a, **k: None
    sys.modules["claw_eval.graders.llm_judge"].LLMJudge = lambda *a, **k: None
    scoring = sys.modules["claw_eval.models.scoring"]
    scoring.compute_task_score = lambda scores: 0.0
    scoring.is_pass = lambda score: False
    sys.modules["claw_eval.models.task"].TaskDefinition = lambda *a, **k: None
    sys.modules["claw_eval.trace.reader"].load_trace = lambda *a, **k: None
    sys.modules["claw_eval"].graders = sys.modules["claw_eval.graders"]
    sys.modules["claw_eval.graders"].registry = (
        sys.modules["claw_eval.graders.registry"])
    sys.modules["claw_eval.graders"].llm_judge = (
        sys.modules["claw_eval.graders.llm_judge"])
    sys.modules["claw_eval"].models = sys.modules["claw_eval.models"]
    sys.modules["claw_eval.models"].scoring = (
        sys.modules["claw_eval.models.scoring"])
    sys.modules["claw_eval.models"].task = sys.modules["claw_eval.models.task"]
    sys.modules["claw_eval"].trace = sys.modules["claw_eval.trace"]
    sys.modules["claw_eval.trace"].reader = sys.modules["claw_eval.trace.reader"]

    try:
        from ce_runner import grader_runner
        yield grader_runner
    finally:
        for name, mod in saved.items():
            if mod is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = mod


class TestJudgeConfigAssembly:
    """main() must not pass an all-empty judge config to grade_trace."""

    def test_unset_judge_args_yield_no_judge_config(self, grader_mod,
                                                    tmp_path, monkeypatch):
        """No CLI args and no env vars -> judge_config must be empty.

        The dict comprehension filtered ``None``, but the ``or`` fallbacks
        already turn unset values into "", so the filter kept an all-empty
        3-key dict — truthy, so grade_trace always constructed a judge with
        an empty model/base_url/api_key.
        """
        monkeypatch.delenv("JUDGE_MODEL_ID", raising=False)
        monkeypatch.delenv("JUDGE_BASE_URL", raising=False)
        monkeypatch.delenv("JUDGE_API_KEY", raising=False)
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\n")
        monkeypatch.setattr(
            sys, "argv",
            ["grader_runner", "--trace", str(tmp_path / "t.jsonl"),
             "--task-yaml", str(task_yaml)])

        captured: dict = {}

        def fake_grade_trace(trace_path, task_yaml_path, judge_config=None,
                             env_snapshot_data=None):
            captured["judge_config"] = judge_config
            return {"task_score": 0.0, "passed": False}

        with patch.object(grader_mod, "grade_trace", fake_grade_trace):
            grader_mod.main()

        assert not captured["judge_config"], (
            "an entirely unset judge config must stay empty so no judge is "
            f"constructed: {captured['judge_config']!r}"
        )

    def test_env_vars_still_reach_the_judge_config(self, grader_mod,
                                                   tmp_path, monkeypatch):
        """Guard: the env-var fallback keeps working after the filter."""
        monkeypatch.setenv("JUDGE_MODEL_ID", "qwen-max")
        monkeypatch.setenv("JUDGE_BASE_URL", "https://judge.example")
        monkeypatch.setenv("JUDGE_API_KEY", "sk-x")
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\n")
        monkeypatch.setattr(
            sys, "argv",
            ["grader_runner", "--trace", str(tmp_path / "t.jsonl"),
             "--task-yaml", str(task_yaml)])

        captured: dict = {}

        def fake_grade_trace(trace_path, task_yaml_path, judge_config=None,
                             env_snapshot_data=None):
            captured["judge_config"] = judge_config
            return {"task_score": 0.0, "passed": False}

        with patch.object(grader_mod, "grade_trace", fake_grade_trace):
            grader_mod.main()

        assert captured["judge_config"] == {
            "model_id": "qwen-max",
            "base_url": "https://judge.example",
            "api_key": "sk-x",
        }
