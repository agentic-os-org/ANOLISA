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

"""Run session: encapsulates the full run lifecycle from settings to report."""

from __future__ import annotations

import logging
import time
from typing import TYPE_CHECKING

from swe_runner.agents import get_agent
from swe_runner.agents.env_checks import check_agent_environment
from swe_runner.common.models import Settings
from swe_runner.run.dataset import load_dataset
from swe_runner.run.execution.orchestrator import Orchestrator
from swe_runner.run.io.output_store import RunOutputStore
from swe_runner.run.io.run_metadata import RunMetadataSnapshot

if TYPE_CHECKING:
    from swe_runner.run.io.report import RunReport

logger = logging.getLogger(__name__)


class RunSession:
    """Encapsulates the full run lifecycle: env check → agent → dataset → batch → metadata."""

    def __init__(self, settings: Settings, *, redo: bool = False) -> None:
        self._settings = settings
        self._redo = redo

    def execute(self) -> RunReport:
        """Execute the run session and return an aggregated report.

        Raises:
            AgentEnvironmentError: if the agent environment check fails.
            KeyError: if the agent name is not recognized.
        """
        from swe_runner.run.io.report import RunReport

        agent_name = self._settings.agent.name
        output_dir = self._settings.output.output_dir

        logger.info("SESSION_ENV_CHECK agent=%s", agent_name)
        check_agent_environment(agent_name)

        logger.info("SESSION_AGENT_RESOLVE agent=%s", agent_name)
        agent_instance = get_agent(agent_name)

        instances = load_dataset(self._settings.dataset)
        logger.info("SESSION_DATASET_READY instances=%s agent=%s", len(instances), agent_name)

        if not instances:
            return RunReport(succeeded=0, failed=0, total=0, instance_ids=[], metadata_path=None)

        orchestrator = Orchestrator(agent_instance, self._settings, redo=self._redo)
        started_at_ns = time.time_ns()
        results = orchestrator.run_batch(instances, output_dir)
        ended_at_ns = time.time_ns()

        report = RunReport.from_results(results, started_at_ns=started_at_ns, ended_at_ns=ended_at_ns)

        metadata_path = RunOutputStore(output_dir).write_run_metadata(
            RunMetadataSnapshot(
                started_at_ns=report.started_at_ns,
                ended_at_ns=report.ended_at_ns,
                agent_name=agent_name,
                workers=self._settings.agent.workers,
                instance_ids=report.instance_ids,
                succeeded=report.succeeded,
                metadata_mappings=report.metadata_mappings,
            )
        )
        report = report.model_copy(update={"metadata_path": metadata_path})

        logger.info(
            "SESSION_END agent=%s succeeded=%s failed=%s total=%s",
            agent_name,
            report.succeeded,
            report.failed,
            report.total,
        )
        return report
