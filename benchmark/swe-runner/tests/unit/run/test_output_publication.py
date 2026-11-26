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

"""Publication failures must leave SWE runner instances eligible for resume."""

from __future__ import annotations

import json
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

import swe_runner.run.io.output_store as output_store
from swe_runner.common.models import AgentResult, InstanceResult, Prediction, SWEInstance
from swe_runner.run.io.output_store import RunOutputStore


def _result(instance_id: str = "case-1", *, success: bool = True, patch: str = "diff") -> InstanceResult:
    return InstanceResult(
        instance=SWEInstance(
            instance_id=instance_id,
            repo="example/repo",
            version="1.0",
            base_commit="abc123",
            problem_statement="Fix the regression",
            patch="",
            test_patch="",
        ),
        prediction=(
            Prediction(instance_id=instance_id, model_name_or_path="cosh", model_patch=patch)
            if success
            else None
        ),
        agent_result=AgentResult(raw_output="done", patch=patch, success=success, duration_seconds=1.0),
        success=success,
    )


@pytest.mark.parametrize("failure", ["write", "replace"])
def test_failed_prediction_is_not_marked_attempted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, failure: str
) -> None:
    store = RunOutputStore(tmp_path)
    store.save_instance_result(_result("complete"))
    previous_predictions = store.predictions_path.read_bytes()
    real_write = Path.write_bytes
    real_replace = output_store.os.replace

    def write(path: Path, *args, **kwargs):
        if failure == "write" and path == store.predictions_path.with_name("preds.json.tmp"):
            real_write(path, b"{")
            raise OSError("prediction write failed")
        return real_write(path, *args, **kwargs)

    def replace(source, destination):
        if failure == "replace" and Path(destination) == store.predictions_path:
            raise OSError("prediction replace failed")
        return real_replace(source, destination)

    monkeypatch.setattr(Path, "write_bytes", write)
    monkeypatch.setattr(output_store.os, "replace", replace)
    with pytest.raises(OSError, match="prediction"):
        store.save_instance_result(_result())

    assert not (store.results_dir / "case-1.json").exists()
    assert store.load_attempted_instance_ids() == {"complete"}
    assert store.predictions_path.read_bytes() == previous_predictions
    assert not list(tmp_path.rglob("*.tmp"))

    monkeypatch.setattr(Path, "write_bytes", real_write)
    monkeypatch.setattr(output_store.os, "replace", real_replace)
    store.save_instance_result(_result())
    assert store.load_attempted_instance_ids() == {"complete", "case-1"}


@pytest.mark.parametrize("existing_summary", [False, True])
def test_failed_result_publication_preserves_the_previous_summary(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, existing_summary: bool
) -> None:
    store = RunOutputStore(tmp_path)
    result_file = store.results_dir / "case-1.json"
    if existing_summary:
        store.save_instance_result(_result(patch="old"))
    previous = result_file.read_bytes() if result_file.exists() else None
    previous_predictions = store.predictions_path.read_bytes() if store.predictions_path.exists() else None
    real_replace = output_store.os.replace

    def replace(source, destination):
        if Path(destination) == result_file:
            raise OSError("result replace failed")
        return real_replace(source, destination)

    monkeypatch.setattr(output_store.os, "replace", replace)
    with pytest.raises(OSError, match="result replace failed"):
        store.save_instance_result(_result(patch="new"))

    assert (result_file.read_bytes() if result_file.exists() else None) == previous
    assert (
        store.predictions_path.read_bytes() if store.predictions_path.exists() else None
    ) == previous_predictions
    assert not list(tmp_path.rglob("*.tmp"))


@pytest.mark.parametrize("predictions", [None, "{}", "{broken"])
def test_resume_retries_legacy_success_without_its_prediction(tmp_path: Path, predictions: str | None) -> None:
    store = RunOutputStore(tmp_path)
    store.write_instance_result_file(_result())
    # A failed agent can leave a patch while deliberately producing no prediction.
    store.write_instance_result_file(_result("failed", success=False))
    (store.results_dir / "legacy.json").write_text('{"instance_id": "legacy"}', encoding="utf-8")
    if predictions is not None:
        store.predictions_path.write_text(predictions, encoding="utf-8")

    assert store.load_attempted_instance_ids() == {"failed", "legacy"}
    store.save_instance_result(_result())
    assert store.load_attempted_instance_ids() == {"case-1", "failed", "legacy"}


def test_rollback_cannot_discard_a_concurrent_instance_prediction(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    store = RunOutputStore(tmp_path)
    store.save_instance_result(_result(patch="old"))
    result_file = store.results_dir / "case-1.json"
    publication_started = threading.Event()
    allow_failure = threading.Event()
    other_started = threading.Event()
    other_finished = threading.Event()
    real_replace = output_store.os.replace

    def replace(source, destination):
        if Path(destination) == result_file:
            publication_started.set()
            if not allow_failure.wait(timeout=5):
                raise TimeoutError("test did not release the failed publication")
            raise OSError("result replace failed")
        return real_replace(source, destination)

    def save_other():
        other_started.set()
        RunOutputStore(tmp_path).save_instance_result(_result("other", patch="other-patch"))
        other_finished.set()

    monkeypatch.setattr(output_store.os, "replace", replace)
    with ThreadPoolExecutor(max_workers=2) as pool:
        failed = pool.submit(store.save_instance_result, _result(patch="new"))
        try:
            assert publication_started.wait(timeout=5)
            other = pool.submit(save_other)
            assert other_started.wait(timeout=5)
            assert not other_finished.wait(timeout=0.05)
        finally:
            allow_failure.set()
        with pytest.raises(OSError, match="result replace failed"):
            failed.result(timeout=5)
        other.result(timeout=5)

    predictions = json.loads(store.predictions_path.read_text(encoding="utf-8"))
    assert predictions["case-1"]["model_patch"] == "old"
    assert predictions["other"]["model_patch"] == "other-patch"
    assert store.load_attempted_instance_ids() == {"case-1", "other"}
    assert not list(tmp_path.rglob("*.tmp"))


def test_failed_rollback_removes_the_successful_resume_marker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    store = RunOutputStore(tmp_path)
    store.save_instance_result(_result(patch="old"))
    store.save_instance_result(_result("complete"))
    real_replace = output_store.os.replace
    prediction_writes = 0

    def replace(source, destination):
        nonlocal prediction_writes
        destination = Path(destination)
        if destination == store.results_dir / "case-1.json":
            raise OSError("result replace failed")
        if destination == store.predictions_path:
            prediction_writes += 1
            if prediction_writes == 2:
                raise OSError("prediction rollback failed")
        return real_replace(source, destination)

    monkeypatch.setattr(output_store.os, "replace", replace)
    with pytest.raises(OSError, match="prediction rollback failed"):
        store.save_instance_result(_result(patch="new"))

    assert store.load_attempted_instance_ids() == {"complete"}
    assert not (store.results_dir / "case-1.json").exists()
    assert not list(tmp_path.rglob("*.tmp"))
