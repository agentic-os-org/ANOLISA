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

"""Grade persistence uses real files with a deterministic offline grader."""

import importlib.util
import json
import os
import stat
import sys
from pathlib import Path
from tempfile import NamedTemporaryFile
from types import ModuleType, SimpleNamespace
from typing import Any

import pytest


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
        completion=0.75,
        robustness=0.8,
        communication=0.9,
        safety=1.0,
        efficiency_turns=2,
        efficiency_tokens=300,
        efficiency_wall_time_s=5.0,
    )

    def grade(*args: Any, **kwargs: Any) -> SimpleNamespace:
        return scores

    modules["claw_eval.graders.registry"].get_grader = lambda *args, **kwargs: (
        SimpleNamespace(grade=grade)
    )
    modules["claw_eval.graders.llm_judge"].LLMJudge = object
    modules["claw_eval.models.scoring"].compute_task_score = lambda value: 0.75
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
    spec = importlib.util.spec_from_file_location("atomic_grader_under_test", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def trace(tmp_path: Path) -> Path:
    path = tmp_path / "trace.jsonl"
    events = [
        {"type": "trace_start", "trace_id": "trace-1", "task_id": "T001"},
        {
            "type": "message",
            "message": {"role": "assistant", "content": "original evidence"},
        },
        {
            "type": "trace_end",
            "scores": {"custom": 7},
            "task_score": 0.0,
            "passed": False,
        },
        {"type": "grading_result", "task_score": 0.0},
    ]
    path.write_text(
        "\n".join(json.dumps(event) for event in events) + "\n", encoding="utf-8"
    )
    return path


def test_serialization_failure_preserves_original_trace(
    grader_module: ModuleType, trace: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    original = trace.read_bytes()
    dumps = json.dumps
    calls = 0

    def fail_once(value: Any, **kwargs: Any) -> str:
        nonlocal calls
        calls += 1
        if calls == 2:
            raise TypeError("injected serialization failure")
        return dumps(value, **kwargs)

    monkeypatch.setattr(grader_module.json, "dumps", fail_once)
    error = None
    try:
        grader_module.grade_trace(str(trace), "task.yaml")
    except TypeError as exc:
        error = exc

    assert trace.read_bytes() == original
    assert error is not None and "injected serialization failure" in str(error)
    assert sorted(path.name for path in trace.parent.iterdir()) == [trace.name]


def test_replacement_failure_preserves_original_trace(
    grader_module: ModuleType, trace: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    original = trace.read_bytes()

    def fail_replace(source: Any, destination: Any) -> None:
        raise PermissionError("injected replacement failure")

    monkeypatch.setattr(grader_module.os, "replace", fail_replace)
    with pytest.raises(PermissionError, match="injected replacement failure"):
        grader_module.grade_trace(str(trace), "task.yaml")
    assert trace.read_bytes() == original
    assert sorted(path.name for path in trace.parent.iterdir()) == [trace.name]


def test_partial_temporary_write_preserves_original_trace(
    grader_module: ModuleType, trace: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    original = trace.read_bytes()

    def failing_temporary(*args: Any, **kwargs: Any) -> Any:
        output = NamedTemporaryFile(*args, **kwargs)
        write = output.write

        def fail_after_partial_write(text: str) -> int:
            write(text[:5])
            raise OSError("injected disk write failure")

        output.write = fail_after_partial_write
        return output

    monkeypatch.setattr(grader_module, "NamedTemporaryFile", failing_temporary)
    with pytest.raises(OSError, match="injected disk write failure"):
        grader_module.grade_trace(str(trace), "task.yaml")
    assert trace.read_bytes() == original
    assert sorted(path.name for path in trace.parent.iterdir()) == [trace.name]


def test_readers_keep_original_until_complete_trace_is_ready(
    grader_module: ModuleType, trace: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    original = trace.read_bytes()
    dumps = json.dumps
    observed = []

    def observe(value: Any, **kwargs: Any) -> str:
        observed.append(trace.read_bytes())
        return dumps(value, **kwargs)

    monkeypatch.setattr(grader_module.json, "dumps", observe)
    grader_module.grade_trace(str(trace), "task.yaml")
    assert observed and all(contents == original for contents in observed)
    assert trace.read_bytes() != original


def test_regrading_preserves_evidence_and_replaces_prior_score(
    grader_module: ModuleType, trace: Path
) -> None:
    for _ in range(2):
        result = grader_module.grade_trace(str(trace), "task.yaml")
        assert result["passed"] is True
    events = [
        json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines()
    ]
    assert [event["type"] for event in events] == [
        "trace_start",
        "message",
        "trace_end",
        "grading_result",
    ]
    assert events[1]["message"]["content"] == "original evidence"
    assert events[2]["scores"]["custom"] == 7
    assert events[2]["task_score"] == events[3]["task_score"] == 0.75
    assert events[2]["passed"] is True
    assert sorted(path.name for path in trace.parent.iterdir()) == [trace.name]


def test_unicode_evidence_remains_utf8(grader_module: ModuleType, trace: Path) -> None:
    contents = trace.read_text(encoding="utf-8").replace(
        "original evidence", "完整工具证据 🌱"
    )
    trace.write_text(contents, encoding="utf-8")
    grader_module.grade_trace(str(trace), "task.yaml")
    events = [
        json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines()
    ]
    assert events[1]["message"]["content"] == "完整工具证据 🌱"


@pytest.mark.skipif(os.name == "nt", reason="POSIX permission bits")
def test_existing_permission_bits_are_retained(
    grader_module: ModuleType, trace: Path
) -> None:
    trace.chmod(0o640)
    grader_module.grade_trace(str(trace), "task.yaml")
    assert stat.S_IMODE(trace.stat().st_mode) == 0o640


@pytest.mark.skipif(
    os.name == "nt", reason="symlink creation requires platform privileges"
)
def test_symlink_trace_keeps_link_and_updates_target(
    grader_module: ModuleType, trace: Path
) -> None:
    alias = trace.with_name("linked.jsonl")
    alias.symlink_to(trace.name)
    grader_module.grade_trace(str(alias), "task.yaml")
    assert alias.is_symlink()
    assert alias.read_bytes() == trace.read_bytes()
    assert (
        json.loads(trace.read_text(encoding="utf-8").splitlines()[-1])["type"]
        == "grading_result"
    )
