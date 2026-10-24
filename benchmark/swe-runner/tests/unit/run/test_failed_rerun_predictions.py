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

"""The evaluator sees predictions from the latest saved attempt only."""

import json
from pathlib import Path

from swe_runner.common.models import AgentResult, InstanceResult, Prediction, SWEInstance
from swe_runner.run.io.output_store import RunOutputStore


def _result(instance: SWEInstance, *, success: bool) -> InstanceResult:
    patch = "new patch" if success else None
    return InstanceResult(
        instance=instance,
        prediction=Prediction(instance_id=instance.instance_id, model_name_or_path="cosh", model_patch=patch)
        if patch
        else None,
        agent_result=AgentResult(raw_output="", patch=patch, success=success, duration_seconds=1),
        success=success,
    )


def test_failed_rerun_removes_its_previous_prediction(tmp_path: Path, sample_instance: SWEInstance) -> None:
    store = RunOutputStore(tmp_path)
    store.save_instance_result(_result(sample_instance, success=True))
    store.write_prediction(Prediction(instance_id="other", model_name_or_path="cosh", model_patch="keep patch"))

    store.save_instance_result(_result(sample_instance, success=False))

    predictions = json.loads(store.predictions_path.read_text(encoding="utf-8"))
    assert sample_instance.instance_id not in predictions
    assert predictions["other"]["model_patch"] == "keep patch"
    result = json.loads((store.results_dir / f"{sample_instance.instance_id}.json").read_text(encoding="utf-8"))
    assert result["success"] is False


def test_first_failed_attempt_does_not_create_predictions(tmp_path: Path, sample_instance: SWEInstance) -> None:
    store = RunOutputStore(tmp_path)

    store.save_instance_result(_result(sample_instance, success=False))

    assert not store.predictions_path.exists()


def test_failed_rerun_of_only_prediction_leaves_valid_empty_mapping(tmp_path: Path, sample_instance: SWEInstance) -> None:
    store = RunOutputStore(tmp_path)
    store.save_instance_result(_result(sample_instance, success=True))

    store.save_instance_result(_result(sample_instance, success=False))
    store.save_instance_result(_result(sample_instance, success=False))

    assert json.loads(store.predictions_path.read_text(encoding="utf-8")) == {}


def test_missing_prediction_leaves_other_entries_unchanged(tmp_path: Path, sample_instance: SWEInstance) -> None:
    store = RunOutputStore(tmp_path)
    store.write_prediction(Prediction(instance_id="other", model_name_or_path="cosh", model_patch="keep patch"))
    before = store.predictions_path.read_bytes()
    modified_at = store.predictions_path.stat().st_mtime_ns

    store.save_instance_result(_result(sample_instance, success=False))

    assert store.predictions_path.read_bytes() == before
    assert store.predictions_path.stat().st_mtime_ns == modified_at
