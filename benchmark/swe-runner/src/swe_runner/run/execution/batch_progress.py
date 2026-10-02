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

"""Batch progress reporting for runner orchestration."""

from __future__ import annotations

import logging
import threading
from types import TracebackType
from typing import Protocol

from rich.progress import BarColumn, Progress, SpinnerColumn, TaskID, TaskProgressColumn, TextColumn, TimeElapsedColumn

from swe_runner.common.models import InstanceResult

logger = logging.getLogger(__name__)


class BatchProgressReporter(Protocol):
    """Progress reporter interface used by the batch orchestrator."""

    def __enter__(self) -> BatchProgressReporter:
        """Start progress reporting."""
        ...

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        """Stop progress reporting."""
        ...

    def record_completion(self, result: InstanceResult, *, workers: int) -> None:
        """Record one completed instance result."""
        ...


class RichBatchProgress:
    """Rich-backed progress reporter for batch runs."""

    def __init__(self, total_instances: int) -> None:
        self._total_instances = total_instances
        self._progress = Progress(
            SpinnerColumn(),
            TextColumn("[progress.description]{task.description}"),
            BarColumn(),
            TaskProgressColumn(),
            TimeElapsedColumn(),
        )
        self._task_id: TaskID | None = None
        self._completed_count = 0
        self._lock = threading.Lock()

    def __enter__(self) -> RichBatchProgress:
        self._progress.__enter__()
        self._task_id = self._progress.add_task("Processing instances", total=self._total_instances)
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> None:
        self._progress.__exit__(exc_type, exc_val, exc_tb)

    def record_completion(self, result: InstanceResult, *, workers: int) -> None:
        """Update progress state for one completed instance."""
        if self._task_id is None:
            raise RuntimeError("Batch progress reporter has not been started")

        with self._lock:
            self._completed_count += 1
            status = "\u2713" if result.success else "\u2717"
            duration = result.agent_result.duration_seconds
            self._progress.update(
                self._task_id,
                description=(
                    f"[{self._completed_count}/{self._total_instances}] "
                    f"{result.instance.instance_id} {status} ({duration:.1f}s)"
                ),
            )
            self._progress.advance(self._task_id)
            logger.info(
                "BATCH_PROGRESS index=%s/%s instance=%s workers=%s thread=%s success=%s duration=%.2fs",
                self._completed_count,
                self._total_instances,
                result.instance.instance_id,
                workers,
                threading.current_thread().name,
                result.success,
                duration,
            )
