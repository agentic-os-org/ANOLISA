"""Latest failed attempts must invalidate only their own evaluable prediction."""

from __future__ import annotations

import json
from concurrent.futures import ThreadPoolExecutor
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.agents import AgentAdapter, PreparedAgentRun
from swe_runner.common.models import AgentResult, InstanceResult, Prediction, Settings, SWEInstance
from swe_runner.evaluation.service import _get_instance_ids
from swe_runner.run.execution.orchestrator import Orchestrator
from swe_runner.run.io.output_store import RunOutputStore


def _instance(instance_id: str) -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="example/repo",
        version="1",
        base_commit="abc",
        problem_statement="Fix the bug",
        patch="",
        test_patch="",
    )


def _prediction(instance_id: str, patch_text: str = "old patch") -> Prediction:
    return Prediction(instance_id=instance_id, model_name_or_path="fixture", model_patch=patch_text)


def _failure(instance_id: str) -> InstanceResult:
    return InstanceResult(
        instance=_instance(instance_id),
        prediction=None,
        agent_result=AgentResult(raw_output="failed", patch=None, success=False, duration_seconds=0, error="failed"),
        success=False,
    )


class _FixtureAdapter(AgentAdapter):
    def __init__(self, work_dir: Path, failure: str | None = None) -> None:
        self.work_dir = work_dir
        self.failure = failure

    @property
    def name(self) -> str:
        return "fixture"

    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        if self.failure == "prepare":
            raise RuntimeError("prepare failed")
        return PreparedAgentRun(
            instance=instance, settings=settings, work_dir=self.work_dir, prompt="test", timeout=1, max_turns=1
        )

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        if self.failure == "agent":
            raise RuntimeError("agent failed")
        return AgentResult(raw_output="ok", patch=None, success=True, duration_seconds=0)


class _QuietProgress:
    def record_completion(self, result: InstanceResult, *, workers: int) -> None:
        pass


def _progress_factory(total: int) -> nullcontext[_QuietProgress]:
    return nullcontext(_QuietProgress())


@pytest.mark.parametrize("failure", ["prepare", "agent", "empty_patch", "postprocess"])
def test_failed_redo_removes_only_its_previous_prediction(tmp_path: Path, failure: str) -> None:
    instance = _instance("redo-1")
    output = tmp_path / "run"
    store = RunOutputStore(output)
    other = _prediction("untouched-2", "unrelated patch")
    with (
        patch("swe_runner.run.execution.instance_lifecycle.write_input_manifest", return_value=tmp_path / "manifest"),
        patch("swe_runner.run.workspace.patches.extract_patch", return_value="first successful patch"),
    ):
        first = Orchestrator(_FixtureAdapter(tmp_path), progress_factory=_progress_factory).run_batch(
            [instance], output
        )
    assert first[0].success
    store.write_prediction(other)

    with (
        patch("swe_runner.run.execution.instance_lifecycle.write_input_manifest", return_value=tmp_path / "manifest"),
        patch(
            "swe_runner.run.workspace.patches.extract_patch",
            return_value=None if failure == "empty_patch" else "failed attempt patch",
            side_effect=RuntimeError("postprocessing failed") if failure == "postprocess" else None,
        ),
    ):
        latest = Orchestrator(
            _FixtureAdapter(tmp_path, failure), redo=True, progress_factory=_progress_factory
        ).run_batch([instance], output)

    assert not latest[0].success
    assert latest[0].prediction is None
    summary = json.loads((store.results_dir / "redo-1.json").read_text(encoding="utf-8"))
    assert summary["success"] is False
    predictions = json.loads(store.predictions_path.read_text(encoding="utf-8"))
    assert predictions == {other.instance_id: other.model_dump()}
    assert _get_instance_ids(store.predictions_path) == [other.instance_id]


def test_failed_first_attempt_does_not_create_predictions(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.save_instance_result(_failure("new-1"))
    assert not store.predictions_path.exists()


def test_failed_attempt_absent_from_predictions_keeps_existing_file(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.write_prediction(_prediction("other"))
    before = store.predictions_path.read_bytes()
    store.save_instance_result(_failure("absent"))
    assert store.predictions_path.read_bytes() == before


def test_failed_redo_of_only_prediction_leaves_empty_evaluation_input(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.write_prediction(_prediction("redo-1"))
    store.save_instance_result(_failure("redo-1"))
    assert json.loads(store.predictions_path.read_text(encoding="utf-8")) == {}
    assert _get_instance_ids(store.predictions_path) == []


def test_failed_redo_and_other_prediction_writes_share_the_lock(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.write_prediction(_prediction("redo-1"))
    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(store.save_instance_result, _failure("redo-1"))]
        futures.extend(pool.submit(store.write_prediction, _prediction(f"other-{number}")) for number in range(20))
        for future in futures:
            future.result()
    predictions = json.loads(store.predictions_path.read_text(encoding="utf-8"))
    assert set(predictions) == {f"other-{number}" for number in range(20)}
