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
from pathlib import Path

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


def _prediction(instance_id: str, patch: str = "diff") -> Prediction:
    return Prediction(
        instance_id=instance_id,
        model_name_or_path="cosh",
        model_patch=patch,
    )


def _result(instance_id: str) -> InstanceResult:
    return InstanceResult(
        instance=_instance(instance_id),
        prediction=_prediction(instance_id),
        agent_result=AgentResult(raw_output="ok", patch="diff", success=True, duration_seconds=1.0),
        success=True,
    )


def test_save_instance_result_survives_truncated_preds(tmp_path: Path) -> None:
    """A truncated preds.json (crash/disk-full mid-write) must not raise.

    One corrupt file previously killed every later save_instance_result
    with an uncaught JSONDecodeError and wedged all resumed runs; the store
    must mirror load_attempted_instance_ids' tolerance and self-heal.
    """
    tmp_path.joinpath("preds.json").write_text(
        '{"inst-1": {"instance_id": "inst-1", "model_na', encoding="utf-8"
    )
    store = RunOutputStore(tmp_path)

    store.save_instance_result(_result("inst-2"))  # must not raise

    predictions = json.loads((tmp_path / "preds.json").read_text(encoding="utf-8"))
    assert "inst-2" in predictions


def test_write_prediction_non_object_json_treated_as_empty(tmp_path: Path) -> None:
    """A valid-JSON-but-not-an-object preds.json also self-heals."""
    tmp_path.joinpath("preds.json").write_text("[]", encoding="utf-8")
    store = RunOutputStore(tmp_path)

    store.write_prediction(_prediction("inst-1"))

    predictions = json.loads((tmp_path / "preds.json").read_text(encoding="utf-8"))
    assert set(predictions) == {"inst-1"}


def test_write_prediction_is_atomic_no_tmp_left(tmp_path: Path) -> None:
    """Successive writes leave no temp files and produce valid JSON."""
    store = RunOutputStore(tmp_path)

    store.write_prediction(_prediction("inst-1", patch="one"))
    store.write_prediction(_prediction("inst-2", patch="two"))

    leftovers = list(tmp_path.glob("*.tmp")) + list(tmp_path.glob(".*tmp*"))
    assert leftovers == []
    predictions = json.loads((tmp_path / "preds.json").read_text(encoding="utf-8"))
    assert set(predictions) == {"inst-1", "inst-2"}
    assert predictions["inst-1"]["model_patch"] == "one"
    assert predictions["inst-2"]["model_patch"] == "two"


def test_write_prediction_failed_replace_keeps_previous_file(tmp_path: Path) -> None:
    """A failure at the replace step keeps the previous complete file and
    leaves no temp file behind."""
    from unittest.mock import patch

    store = RunOutputStore(tmp_path)
    store.write_prediction(_prediction("inst-1", patch="one"))

    with patch("swe_runner.run.io.output_store.os.replace", side_effect=OSError("disk full")):
        try:
            store.write_prediction(_prediction("inst-2", patch="two"))
        except OSError:
            pass  # the write error itself propagates

    leftovers = list(tmp_path.glob("*.tmp")) + list(tmp_path.glob(".*tmp*"))
    assert leftovers == []
    predictions = json.loads((tmp_path / "preds.json").read_text(encoding="utf-8"))
    assert set(predictions) == {"inst-1"}  # previous complete file intact
