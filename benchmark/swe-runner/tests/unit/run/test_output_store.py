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

"""Tests for the run output store."""

from __future__ import annotations

import json
import multiprocessing
import threading
from concurrent.futures import ThreadPoolExecutor
from multiprocessing.synchronize import Event
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest

from swe_runner.common.models import AgentResult, InstanceResult, Prediction, SWEInstance
from swe_runner.run.io.output_store import RunOutputStore
from swe_runner.run.io.run_metadata import RunMetadataSnapshot


def _instance(instance_id: str = "inst-1") -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="example/repo",
        version="1.0",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )


def test_save_instance_result_writes_result_and_prediction(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    result = InstanceResult(
        instance=_instance(),
        prediction=Prediction(instance_id="inst-1", model_name_or_path="cosh", model_patch="diff"),
        agent_result=AgentResult(
            raw_output="ok",
            patch="diff",
            success=True,
            duration_seconds=1.5,
            metadata={"session_id": "sess-1"},
        ),
        success=True,
    )

    store.save_instance_result(result)

    result_payload = json.loads((tmp_path / "results" / "inst-1.json").read_text(encoding="utf-8"))
    predictions = json.loads((tmp_path / "preds.json").read_text(encoding="utf-8"))
    assert result_payload["instance_id"] == "inst-1"
    assert result_payload["session_id"] == "sess-1"
    assert predictions["inst-1"]["model_patch"] == "diff"


def test_load_attempted_instance_ids_ignores_invalid_result_files(tmp_path: Path) -> None:
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "valid.json").write_text('{"instance_id": "inst-1"}', encoding="utf-8")
    (results_dir / "invalid.json").write_text("{not-json", encoding="utf-8")

    assert RunOutputStore(tmp_path).load_attempted_instance_ids() == {"inst-1"}


def test_write_run_metadata_merges_existing_payload(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.write_run_metadata(
        RunMetadataSnapshot(
            started_at_ns=10,
            ended_at_ns=20,
            agent_name="cosh",
            workers=1,
            instance_ids=["inst-1"],
            succeeded=1,
        )
    )

    metadata_path = store.write_run_metadata(
        RunMetadataSnapshot(
            started_at_ns=30,
            ended_at_ns=40,
            agent_name="cosh",
            workers=2,
            instance_ids=["inst-2"],
            succeeded=0,
            metadata_mappings={"session_ids": {"inst-2": "sess-2"}},
        )
    )

    payload = json.loads(metadata_path.read_text(encoding="utf-8"))
    assert payload["instance_ids"] == ["inst-1", "inst-2"]
    assert payload["attempt_count"] == 2
    assert payload["run_count"] == 2
    assert payload["session_ids"] == {"inst-2": "sess-2"}


def _snapshot(instance_id: str) -> RunMetadataSnapshot:
    return RunMetadataSnapshot(
        started_at_ns=10,
        ended_at_ns=20,
        agent_name="cosh",
        workers=1,
        instance_ids=[instance_id],
        succeeded=1,
        metadata_mappings={"session_ids": {instance_id: f"session-{instance_id}"}},
    )


def _write_metadata_in_process(
    output_dir: Path,
    instance_id: str,
    started: Event,
    read_entered: Event,
    release_read: Event | None,
) -> None:
    store = RunOutputStore(output_dir)
    original_load = store._load_existing_run_metadata

    def load() -> dict[str, Any] | None:
        payload = original_load()
        read_entered.set()
        if release_read is not None and not release_read.wait(10):
            raise TimeoutError("Timed out waiting to release the first metadata reader")
        return payload

    store._load_existing_run_metadata = load
    started.set()
    store.write_run_metadata(_snapshot(instance_id))


def _assert_three_batches(output_dir: Path) -> None:
    payload = json.loads((output_dir / "run_metadata.json").read_text(encoding="utf-8"))
    assert payload["run_count"] == 3
    assert payload["attempt_count"] == 3
    assert payload["succeeded"] == 3
    assert payload["failed"] == 0
    assert set(payload["instance_ids"]) == {"seed", "first", "second"}
    assert payload["session_ids"] == {name: f"session-{name}" for name in ("seed", "first", "second")}


def test_write_run_metadata_serializes_separate_processes(tmp_path: Path) -> None:
    RunOutputStore(tmp_path).write_run_metadata(_snapshot("seed"))
    context = multiprocessing.get_context("spawn")
    first_started, first_read, release_first = (context.Event() for _ in range(3))
    second_started, second_read = (context.Event() for _ in range(2))
    first = context.Process(
        target=_write_metadata_in_process,
        args=(tmp_path, "first", first_started, first_read, release_first),
    )
    second = context.Process(
        target=_write_metadata_in_process,
        args=(tmp_path, "second", second_started, second_read, None),
    )
    processes = []
    try:
        first.start()
        processes.append(first)
        assert first_read.wait(10)
        second.start()
        processes.append(second)
        assert second_started.wait(10)
        # The first process holds the read/merge/write transaction open.
        assert not second_read.wait(0.5)
    finally:
        release_first.set()
        for process in processes:
            process.join(10)
            if process.is_alive():
                process.terminate()
                process.join(5)
    assert all(process.exitcode == 0 for process in processes)
    assert second_read.is_set()
    _assert_three_batches(tmp_path)


def test_write_run_metadata_serializes_separate_thread_stores(tmp_path: Path) -> None:
    RunOutputStore(tmp_path).write_run_metadata(_snapshot("seed"))
    first_read, release_first, second_started, second_read = (threading.Event() for _ in range(4))
    first_store, second_store = RunOutputStore(tmp_path), RunOutputStore(tmp_path)
    first_load, second_load = first_store._load_existing_run_metadata, second_store._load_existing_run_metadata

    def held_load() -> dict[str, Any] | None:
        payload = first_load()
        first_read.set()
        if not release_first.wait(10):
            raise TimeoutError("Timed out waiting to release the first metadata reader")
        return payload

    def observed_load() -> dict[str, Any] | None:
        second_read.set()
        return second_load()

    def second_write() -> Path:
        second_started.set()
        return second_store.write_run_metadata(_snapshot("second"))

    with (
        patch.object(first_store, "_load_existing_run_metadata", held_load),
        patch.object(second_store, "_load_existing_run_metadata", observed_load),
        ThreadPoolExecutor(max_workers=2) as executor,
    ):
        first = executor.submit(first_store.write_run_metadata, _snapshot("first"))
        try:
            assert first_read.wait(10)
            second = executor.submit(second_write)
            assert second_started.wait(10)
            assert not second_read.wait(0.5)
        finally:
            release_first.set()
        first.result(timeout=10)
        second.result(timeout=10)
    _assert_three_batches(tmp_path)


def test_write_run_metadata_publishes_complete_json_atomically(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.write_run_metadata(_snapshot("seed"))
    old_bytes = store.run_metadata_path.read_bytes()
    store.run_metadata_path.chmod(0o640)
    original_replace = Path.replace
    replacements = []

    def observed_replace(source: Path, target: Path) -> Path:
        assert source.parent == target.parent == tmp_path
        assert target.read_bytes() == old_bytes
        assert json.loads(source.read_text(encoding="utf-8"))["run_count"] == 2
        replacements.append(source)
        return original_replace(source, target)

    with patch.object(Path, "replace", observed_replace):
        store.write_run_metadata(_snapshot("first"))
    assert len(replacements) == 1
    assert json.loads(store.run_metadata_path.read_text(encoding="utf-8"))["run_count"] == 2
    assert store.run_metadata_path.stat().st_mode & 0o777 == 0o640
    assert not list(tmp_path.glob(".run_metadata.*.tmp"))


def test_write_run_metadata_replace_failure_retains_file_and_releases_lock(tmp_path: Path) -> None:
    store = RunOutputStore(tmp_path)
    store.write_run_metadata(_snapshot("seed"))
    old_bytes = store.run_metadata_path.read_bytes()
    with (
        patch.object(Path, "replace", side_effect=OSError("replacement failed")),
        pytest.raises(OSError, match="replacement failed"),
    ):
        store.write_run_metadata(_snapshot("first"))
    assert store.run_metadata_path.read_bytes() == old_bytes
    assert not list(tmp_path.glob(".run_metadata.*.tmp"))
    RunOutputStore(tmp_path).write_run_metadata(_snapshot("second"))
    payload = json.loads(store.run_metadata_path.read_text(encoding="utf-8"))
    assert payload["run_count"] == 2
    assert payload["instance_ids"] == ["seed", "second"]


def test_write_run_metadata_does_not_lock_other_output_directories(tmp_path: Path) -> None:
    first_store = RunOutputStore(tmp_path / "first")
    second_store = RunOutputStore(tmp_path / "second")
    first_read, release_first = threading.Event(), threading.Event()
    original_load = first_store._load_existing_run_metadata

    def held_load() -> dict[str, Any] | None:
        payload = original_load()
        first_read.set()
        if not release_first.wait(10):
            raise TimeoutError("Timed out waiting to release the first metadata reader")
        return payload

    with (
        patch.object(first_store, "_load_existing_run_metadata", held_load),
        ThreadPoolExecutor(max_workers=2) as executor,
    ):
        first = executor.submit(first_store.write_run_metadata, _snapshot("first"))
        try:
            assert first_read.wait(10)
            second = executor.submit(second_store.write_run_metadata, _snapshot("second"))
            assert second.result(timeout=3) == second_store.run_metadata_path
        finally:
            release_first.set()
        first.result(timeout=10)
    assert json.loads(first_store.run_metadata_path.read_text())["instance_ids"] == ["first"]
    assert json.loads(second_store.run_metadata_path.read_text())["instance_ids"] == ["second"]
