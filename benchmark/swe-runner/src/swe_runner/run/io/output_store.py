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

"""Persistent output storage for runner results."""

from __future__ import annotations

import json
import logging
import threading
from pathlib import Path
from typing import Any

from swe_runner.common.models import InstanceResult, Prediction
from swe_runner.run.io.instance_result_summary import build_instance_result_summary
from swe_runner.run.io.run_metadata import RunMetadataSnapshot, merge_run_metadata

logger = logging.getLogger(__name__)

_PREDICTIONS_FILE = "preds.json"
_RESULTS_DIR = "results"
_RUN_METADATA_FILE = "run_metadata.json"
_predictions_lock = threading.Lock()


class RunOutputStore:
    """Filesystem-backed store for one run output directory."""

    def __init__(self, output_dir: Path) -> None:
        self.output_dir = output_dir
        self.predictions_path = output_dir / _PREDICTIONS_FILE
        self.results_dir = output_dir / _RESULTS_DIR
        self.run_metadata_path = output_dir / _RUN_METADATA_FILE

    def write_prediction(self, prediction: Prediction) -> None:
        """Write or replace one SWE-bench prediction in ``preds.json``."""
        self.output_dir.mkdir(parents=True, exist_ok=True)
        logger.info(
            "OUTPUT_WRITE_PREDICTION_START instance=%s preds_file=%s",
            prediction.instance_id,
            self.predictions_path,
        )

        with _predictions_lock:
            if self.predictions_path.exists():
                predictions = json.loads(self.predictions_path.read_text(encoding="utf-8"))
            else:
                predictions = {}

            predictions[prediction.instance_id] = prediction.model_dump()
            self.predictions_path.write_text(json.dumps(predictions, indent=2), encoding="utf-8")

        logger.info(
            "OUTPUT_WRITE_PREDICTION_END instance=%s total_predictions=%s",
            prediction.instance_id,
            len(predictions),
        )

    def write_run_metadata(self, snapshot: RunMetadataSnapshot) -> Path:
        """Write batch-level run timing metadata for later trace export."""
        self.output_dir.mkdir(parents=True, exist_ok=True)
        payload = merge_run_metadata(self._load_existing_run_metadata(), snapshot.to_payload())
        self.run_metadata_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        logger.info(
            "OUTPUT_WRITE_RUN_METADATA file=%s started_at_ns=%s ended_at_ns=%s instances=%s runs=%s",
            self.run_metadata_path,
            snapshot.started_at_ns,
            snapshot.ended_at_ns,
            len(snapshot.instance_ids),
            payload.get("run_count"),
        )
        return self.run_metadata_path

    def write_instance_result_file(self, result: InstanceResult) -> None:
        """Write a per-instance result summary under ``results/<instance_id>.json``."""
        self.results_dir.mkdir(parents=True, exist_ok=True)
        summary = build_instance_result_summary(result)
        result_file = self.results_dir / f"{result.instance.instance_id}.json"
        result_file.write_text(json.dumps(summary, indent=2), encoding="utf-8")

        logger.info(
            "OUTPUT_WRITE_INSTANCE_RESULT instance=%s success=%s patch_produced=%s file=%s",
            summary["instance_id"],
            summary["success"],
            summary["patch_produced"],
            result_file,
        )

    def load_attempted_instance_ids(self) -> set[str]:
        """Load instance IDs that already have per-instance result files."""
        if not self.results_dir.exists():
            logger.info("OUTPUT_LOAD_ATTEMPTED_IDS instance=global count=0 output_dir=%s", self.output_dir)
            return set()

        ids: set[str] = set()
        for result_file in self.results_dir.glob("*.json"):
            try:
                data = json.loads(result_file.read_text(encoding="utf-8"))
                instance_id = data.get("instance_id") if isinstance(data, dict) else None
                if isinstance(instance_id, str) and instance_id:
                    ids.add(instance_id)
            except (json.JSONDecodeError, OSError):
                logger.warning("OUTPUT_LOAD_ATTEMPTED_IDS_SKIP file=%s", result_file)

        logger.info("OUTPUT_LOAD_ATTEMPTED_IDS instance=global count=%s results_dir=%s", len(ids), self.results_dir)
        return ids

    def save_instance_result(self, result: InstanceResult) -> None:
        """Persist one instance result and its prediction when present."""
        logger.info(
            "OUTPUT_SAVE_START instance=%s success=%s has_prediction=%s output_dir=%s",
            result.instance.instance_id,
            result.success,
            result.prediction is not None,
            self.output_dir,
        )
        self.write_instance_result_file(result)
        if result.prediction:
            self.write_prediction(result.prediction)
        logger.info("OUTPUT_SAVE_END instance=%s", result.instance.instance_id)

    def _load_existing_run_metadata(self) -> dict[str, Any] | None:
        if not self.run_metadata_path.exists():
            return None
        try:
            payload = load_run_metadata(self.run_metadata_path)
        except (OSError, json.JSONDecodeError, ValueError) as exc:
            logger.warning("RUN_METADATA_LOAD_EXISTING_FAILED file=%s error=%s", self.run_metadata_path, exc)
            return None
        return payload


def load_run_metadata(path: Path) -> dict[str, Any]:
    """Load run metadata JSON from disk."""
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"Run metadata must be a JSON object: {path}")
    return payload
