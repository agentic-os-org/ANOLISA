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

"""Test grader_runner trace_end score merging.

Covers:
- Converter-computed efficiency_* values in trace_end.scores survive grading
- Grader-set rubric dims still merge into trace_end.scores
- grading_result event records the grader's raw output (incl. its default
  efficiency zeros) unchanged
- Idempotent re-grading keeps exactly one grading_result event

The real claw-eval grader stack (claw_eval.*) is not shipped in this tree;
a minimal stub package — same shape as the audit harness stub — is written
into tmp_path and prepended to sys.path so grader_runner imports against it.
"""

import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

_STUB_FILES = {
    "claw_eval/__init__.py": "",
    "claw_eval/graders/__init__.py": "",
    "claw_eval/graders/registry.py": '''
from dataclasses import dataclass


@dataclass
class Scores:
    completion: float = 0.0
    robustness: float = 0.0
    communication: float = 0.0
    safety: float = 0.0
    efficiency_turns: float = 0.0      # grader never fills these (defaults)
    efficiency_tokens: float = 0.0
    efficiency_wall_time_s: float = 0.0


class FakeGrader:
    """Scores the 4 rubric dimensions and leaves efficiency_* at dataclass
    defaults — grade() receives no session totals, so it cannot compute
    them (mirrors the real graders)."""

    def grade(self, messages, dispatches, task, **kwargs):
        return Scores(completion=1.0, robustness=1.0, communication=1.0, safety=1.0)


def get_grader(task_id, tasks_dir=None, task_dir=None):
    return FakeGrader()
''',
    "claw_eval/graders/llm_judge.py": '''
class LLMJudge:
    def __init__(self, model_id="", api_key=None, base_url=""):
        pass
''',
    "claw_eval/models/__init__.py": "",
    "claw_eval/models/scoring.py": '''
def compute_task_score(scores):
    return (scores.completion + scores.robustness
            + scores.communication + scores.safety) / 4


def is_pass(score):
    return score >= 0.75
''',
    "claw_eval/models/task.py": '''
from pathlib import Path

import yaml


class TaskDefinition:
    def __init__(self, task_id):
        self.task_id = task_id

    @classmethod
    def from_yaml(cls, path):
        data = yaml.safe_load(Path(path).read_text()) or {}
        return cls(data.get("task_id", ""))
''',
    "claw_eval/trace/__init__.py": "",
    "claw_eval/trace/reader.py": '''
import json


class _O:
    def __init__(self, **kw):
        self.__dict__.update(kw)


def load_trace(path):
    events = []
    with open(path) as f:
        for line in f:
            if line.strip():
                events.append(json.loads(line))
    start = next(e for e in events if e.get("type") == "trace_start")
    msgs = [e for e in events if e.get("type") == "message"]
    disp = [e for e in events if e.get("type") == "tool_dispatch"]
    end = next(e for e in events if e.get("type") == "trace_end")
    return _O(trace_id=start["trace_id"], task_id=start["task_id"]), msgs, disp, [], end, {}
''',
}

_CONVERTER_EFFICIENCY = {
    "efficiency_turns": 7,
    "efficiency_tokens": 5300,
    "efficiency_wall_time_s": 412.5,
}


def _purge_modules():
    for mod in list(sys.modules):
        if mod.startswith("claw_eval") or mod == "ce_runner.grader_runner":
            sys.modules.pop(mod, None)


@pytest.fixture
def stub_claw_eval(tmp_path, monkeypatch):
    root = tmp_path / "claw_eval_stub"
    for rel, src in _STUB_FILES.items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(src)
    monkeypatch.syspath_prepend(str(root))
    _purge_modules()
    yield root
    _purge_modules()


def _write_trace(tmp_path, with_grading_result=False):
    trace_file = tmp_path / "trace.jsonl"
    events = [
        {"type": "trace_start", "trace_id": "t-123", "task_id": "T001_demo",
         "model": "openclaw", "timestamp": "2026-10-01T10:00:00+00:00"},
        {"type": "message", "trace_id": "t-123",
         "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]},
         "timestamp": "2026-10-01T10:00:01+00:00"},
        {"type": "trace_end", "trace_id": "t-123", "total_turns": 7,
         "model_input_tokens": 3200, "model_output_tokens": 2100,
         "wall_time_s": 412.5,
         "scores": {"completion": 0.0, "robustness": 0.0, "communication": 0.0,
                    "safety": 1.0, **_CONVERTER_EFFICIENCY},
         "task_score": 0.0, "passed": False,
         "timestamp": "2026-10-01T10:06:52+00:00"},
    ]
    if with_grading_result:
        events.append({"type": "grading_result", "trace_id": "t-123",
                       "task_id": "T001_demo", "scores": {}, "timestamp": "x"})
    trace_file.write_text("".join(json.dumps(e) + "\n" for e in events))

    task_yaml = tmp_path / "task.yaml"
    task_yaml.write_text("task_id: T001_demo\n")
    return trace_file, task_yaml


def _grade(tmp_path):
    from ce_runner.grader_runner import grade_trace

    trace_file, task_yaml = _write_trace(tmp_path)
    result = grade_trace(str(trace_file), str(task_yaml), judge_config=None)
    events = [json.loads(l) for l in trace_file.read_text().splitlines() if l]
    trace_end = next(e for e in events if e.get("type") == "trace_end")
    grading = [e for e in events if e.get("type") == "grading_result"]
    return result, trace_end, grading


class TestTraceEndScoreMerge:
    """trace_end.scores: rubric dims merge, efficiency_* must survive."""

    def test_converter_efficiency_survives_grading(self, stub_claw_eval, tmp_path):
        """A grader returning default 0.0 efficiency must not clobber the
        converter-computed efficiency_* in trace_end.scores."""
        _, trace_end, _ = _grade(tmp_path)
        for key, value in _CONVERTER_EFFICIENCY.items():
            assert trace_end["scores"][key] == value

    def test_rubric_dims_still_merge(self, stub_claw_eval, tmp_path):
        """Grader-set rubric dims (and task_score/passed) still land."""
        result, trace_end, _ = _grade(tmp_path)
        assert trace_end["scores"]["completion"] == 1.0
        assert trace_end["scores"]["robustness"] == 1.0
        assert trace_end["scores"]["communication"] == 1.0
        assert trace_end["scores"]["safety"] == 1.0
        assert trace_end["task_score"] == 1.0
        assert trace_end["passed"] is True
        assert result["task_score"] == 1.0
        assert result["passed"] is True

    def test_grading_result_records_raw_grader_output(self, stub_claw_eval, tmp_path):
        """grading_result keeps the grader's own scores, including its
        efficiency defaults — it is the raw grader record, not a merge."""
        _, _, grading = _grade(tmp_path)
        assert len(grading) == 1
        assert grading[0]["scores"]["efficiency_turns"] == 0.0
        assert grading[0]["scores"]["efficiency_tokens"] == 0.0
        assert grading[0]["scores"]["efficiency_wall_time_s"] == 0.0
        assert grading[0]["scores"]["completion"] == 1.0

    def test_regrading_keeps_single_grading_result(self, stub_claw_eval, tmp_path):
        """Re-grading replaces (not duplicates) the grading_result event and
        still preserves converter efficiency."""
        from ce_runner.grader_runner import grade_trace

        trace_file, task_yaml = _write_trace(tmp_path, with_grading_result=True)
        grade_trace(str(trace_file), str(task_yaml), judge_config=None)
        grade_trace(str(trace_file), str(task_yaml), judge_config=None)
        events = [json.loads(l) for l in trace_file.read_text().splitlines() if l]
        grading = [e for e in events if e.get("type") == "grading_result"]
        trace_end = next(e for e in events if e.get("type") == "trace_end")
        assert len(grading) == 1
        assert trace_end["scores"]["efficiency_tokens"] == 5300
