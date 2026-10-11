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

"""Keep comparison results within each backend's execution boundary."""

import importlib.util
import os
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Callable

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "run_task_compare.py"
RUNNERS = ["run_native", "run_ce_runner", "run_native_batch", "run_ce_runner_batch"]


@pytest.fixture
def comparison(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> ModuleType:
    spec = importlib.util.spec_from_file_location("trace_comparison", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "REPO_ROOT", tmp_path)
    monkeypatch.setattr(module, "CLAW_EVAL_DIR", tmp_path / "claw-eval")
    return module


def _trace(comparison: ModuleType, directory: str, task: str, content: str = "old") -> Path:
    parent = comparison.CLAW_EVAL_DIR / "traces" / directory
    parent.mkdir(parents=True, exist_ok=True)
    path = parent / f"{task}_12345678.jsonl"
    path.write_text(content, encoding="utf-8")
    return path


def _invoke(comparison: ModuleType, runner: str, task: str = "T001_example") -> Path | None:
    fn: Callable = getattr(comparison, runner)
    if runner.endswith("_batch"):
        return fn([task])[task]
    return fn(Path(task))


@pytest.mark.parametrize("runner", RUNNERS)
def test_success_without_trace_does_not_reuse_history(
    comparison: ModuleType, monkeypatch: pytest.MonkeyPatch, runner: str
) -> None:
    _trace(comparison, "old-run", "T001_example")
    monkeypatch.setattr(comparison.subprocess, "run", lambda *a, **k: SimpleNamespace(returncode=0))
    assert _invoke(comparison, runner) is None


@pytest.mark.parametrize("runner", RUNNERS)
def test_new_trace_wins_even_when_history_has_a_later_timestamp(
    comparison: ModuleType, monkeypatch: pytest.MonkeyPatch, runner: str
) -> None:
    old = _trace(comparison, "old-run", "T001_example")
    os.utime(old, ns=(4_000_000_000_000_000_000, 4_000_000_000_000_000_000))
    fresh = comparison.CLAW_EVAL_DIR / "traces" / "current-run" / old.name

    def run(*args: object, **kwargs: object) -> SimpleNamespace:
        _trace(comparison, "current-run", "T001_example", "fresh")
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(comparison.subprocess, "run", run)
    assert _invoke(comparison, runner) == fresh


@pytest.mark.parametrize("runner", RUNNERS)
def test_updated_existing_trace_counts_as_current_output(
    comparison: ModuleType, monkeypatch: pytest.MonkeyPatch, runner: str
) -> None:
    existing = _trace(comparison, "existing-run", "T001_example")
    original_times = (existing.stat().st_atime_ns, existing.stat().st_mtime_ns)

    def run(*args: object, **kwargs: object) -> SimpleNamespace:
        existing.write_text("updated trace with more events", encoding="utf-8")
        # Size changes also prove freshness on filesystems with coarse mtimes.
        os.utime(existing, ns=original_times)
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(comparison.subprocess, "run", run)
    assert _invoke(comparison, runner) == existing


@pytest.mark.parametrize("runner", RUNNERS)
def test_failed_backend_does_not_return_even_a_new_trace(
    comparison: ModuleType, monkeypatch: pytest.MonkeyPatch, runner: str
) -> None:
    def run(*args: object, **kwargs: object) -> SimpleNamespace:
        _trace(comparison, "failed-run", "T001_example", "incomplete")
        return SimpleNamespace(returncode=1)

    monkeypatch.setattr(comparison.subprocess, "run", run)
    assert _invoke(comparison, runner) is None


def test_second_backend_cannot_claim_the_first_backends_trace(
    comparison: ModuleType, monkeypatch: pytest.MonkeyPatch
) -> None:
    native = comparison.CLAW_EVAL_DIR / "traces" / "native-run" / "T001_example_12345678.jsonl"

    def run(command: list[str], **kwargs: object) -> SimpleNamespace:
        if command[0] == "claw-eval":
            _trace(comparison, "native-run", "T001_example", "native result")
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(comparison.subprocess, "run", run)
    assert comparison.run_native(Path("T001_example")) == native
    assert comparison.run_ce_runner(Path("T001_example")) is None


@pytest.mark.parametrize("runner", ["run_native_batch", "run_ce_runner_batch"])
def test_batch_reports_each_tasks_current_output(
    comparison: ModuleType, monkeypatch: pytest.MonkeyPatch, runner: str
) -> None:
    tasks = ["T001_example", "T002_example"]
    _trace(comparison, "historical", tasks[1])
    fresh = comparison.CLAW_EVAL_DIR / "traces" / "current" / f"{tasks[0]}_12345678.jsonl"

    def run(*args: object, **kwargs: object) -> SimpleNamespace:
        _trace(comparison, "current", tasks[0], "fresh")
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(comparison.subprocess, "run", run)
    assert getattr(comparison, runner)(tasks) == {tasks[0]: fresh, tasks[1]: None}


def test_direct_latest_lookup_preserves_prefix_and_ordering(comparison: ModuleType) -> None:
    first = _trace(comparison, "native-first", "T001_example")
    second = _trace(comparison, "native-second", "T001_example")
    unrelated = _trace(comparison, "other", "T001_example")
    for index, path in enumerate([first, second, unrelated], 1):
        os.utime(path, ns=(index * 1_000_000_000, index * 1_000_000_000))
    assert comparison._find_latest_trace("T001_example", None) == unrelated
    assert comparison._find_latest_trace("T001_example", "native-") == second
