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
import logging
from pathlib import Path

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


def test_load_attempted_instance_ids_skips_undecodable_result_files(tmp_path: Path) -> None:
    """A result file with invalid UTF-8 must not abort batch resume.

    The strict UTF-8 read raises UnicodeDecodeError, which the per-file
    skip-and-warn guard must cover like it already covers malformed JSON —
    otherwise one damaged results/*.json file aborts run_batch() before
    prepare_batch() or any instance runs.
    """
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "healthy.json").write_text('{"instance_id": "inst-1"}', encoding="utf-8")
    damaged = b'{"instance_id": "inst-2", "raw_output": "\xff"}'
    (results_dir / "damaged.json").write_bytes(damaged)

    assert RunOutputStore(tmp_path).load_attempted_instance_ids() == {"inst-1"}
    # The damaged file keeps its bytes: no replacement decode, no rewrite.
    assert (results_dir / "damaged.json").read_bytes() == damaged


def test_load_attempted_instance_ids_skips_truncated_multibyte_result_files(tmp_path: Path) -> None:
    """A truncated multibyte sequence is not decodable either and must be skipped."""
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "healthy.json").write_text('{"instance_id": "inst-1"}', encoding="utf-8")
    # "\u4e2d" is U+4E2D (E4 B8 AD); keeping only E4 B8 truncates the sequence.
    (results_dir / "truncated.json").write_bytes(b'{"instance_id": "inst-2", "note": "\xe4\xb8"}')

    assert RunOutputStore(tmp_path).load_attempted_instance_ids() == {"inst-1"}


def test_load_attempted_instance_ids_skips_utf16_result_files(tmp_path: Path) -> None:
    """A UTF-16 result file decodes as garbage bytes under strict UTF-8; skip it."""
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "healthy.json").write_text('{"instance_id": "inst-1"}', encoding="utf-8")
    (results_dir / "utf16.json").write_bytes('{"instance_id": "inst-2"}'.encode("utf-16"))

    assert RunOutputStore(tmp_path).load_attempted_instance_ids() == {"inst-1"}


def test_load_attempted_instance_ids_warns_on_undecodable_result_file(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    """The skip must go through the existing warning so operators see which file was skipped."""
    results_dir = tmp_path / "results"
    results_dir.mkdir()
    (results_dir / "healthy.json").write_text('{"instance_id": "inst-1"}', encoding="utf-8")
    (results_dir / "damaged.json").write_bytes(b'{"instance_id": "inst-2", "raw_output": "\xff"}')

    with caplog.at_level(logging.WARNING, logger="swe_runner.run.io.output_store"):
        assert RunOutputStore(tmp_path).load_attempted_instance_ids() == {"inst-1"}

    skip_records = [r for r in caplog.records if r.getMessage().startswith("OUTPUT_LOAD_ATTEMPTED_IDS_SKIP")]
    assert len(skip_records) == 1
    assert "damaged.json" in skip_records[0].getMessage()
    assert "healthy.json" not in skip_records[0].getMessage()


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
