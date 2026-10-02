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

"""Pipeline orchestration for SWE-bench instances."""

from __future__ import annotations

import logging
import threading
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from swe_runner.agents import AgentAdapter
from swe_runner.common.models import AgentConfig, InstanceResult, Settings, SWEInstance
from swe_runner.run.execution.batch_progress import BatchProgressReporter, RichBatchProgress
from swe_runner.run.execution.instance_runner import run_instance
from swe_runner.run.io.output_store import RunOutputStore

logger = logging.getLogger(__name__)


class Orchestrator:
    """Orchestrates the SWE-bench evaluation pipeline."""

    def __init__(
        self,
        agent: AgentAdapter,
        settings: Settings | None = None,
        *,
        redo: bool = False,
        progress_factory: Callable[[int], BatchProgressReporter] = RichBatchProgress,
    ) -> None:
        self._agent = agent
        self._settings = settings or Settings(agent=AgentConfig(name=agent.name))
        self._redo = redo
        self._progress_factory = progress_factory

    def run_single(self, instance: SWEInstance, output_dir: Path) -> InstanceResult:
        """Run one instance through docker, agent, patch extraction, and output."""
        return run_instance(self._agent, self._settings, instance, output_dir)

    def run_batch(self, instances: list[SWEInstance], output_dir: Path) -> list[InstanceResult]:
        """Run instances sequentially or concurrently based on workers setting."""
        if not instances:
            return []

        output_store = RunOutputStore(output_dir)
        if not self._redo:
            attempted = output_store.load_attempted_instance_ids()
            if attempted:
                skipped = [i for i in instances if i.instance_id in attempted]
                instances = [i for i in instances if i.instance_id not in attempted]
                if skipped:
                    logger.info("SKIP_ATTEMPTED instance=global skipped=%s", len(skipped))

        if not instances:
            return []

        self._agent.prepare_batch(instances, self._settings)
        try:
            return self._run_prepared_batch(instances, output_dir)
        finally:
            self._agent.post_batch()

    def _run_prepared_batch(self, instances: list[SWEInstance], output_dir: Path) -> list[InstanceResult]:
        workers = self._settings.agent.workers
        logger.info(
            "BATCH_START instance=global total_instances=%s workers=%s redo=%s output_dir=%s agent=%s",
            len(instances),
            workers,
            self._redo,
            output_dir,
            self._agent.name,
        )

        results: list[InstanceResult] = []
        with self._progress_factory(len(instances)) as progress:

            def _process(instance: SWEInstance) -> InstanceResult:
                logger.info(
                    "BATCH_WORKER_PICK instance=%s workers=%s thread=%s",
                    instance.instance_id,
                    workers,
                    threading.current_thread().name,
                )
                result = self.run_single(instance, output_dir)
                progress.record_completion(result, workers=workers)
                return result

            with ThreadPoolExecutor(max_workers=workers) as executor:
                futures = []
                for instance in instances:
                    logger.info(
                        "BATCH_SUBMIT instance=%s workers=%s",
                        instance.instance_id,
                        workers,
                    )
                    futures.append(executor.submit(_process, instance))
                for future in as_completed(futures):
                    result = future.result()
                    results.append(result)
        logger.info("BATCH_END instance=global completed_results=%s", len(results))
        return results
