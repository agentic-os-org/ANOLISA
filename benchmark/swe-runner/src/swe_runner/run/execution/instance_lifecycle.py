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

"""Single-instance execution lifecycle."""

from __future__ import annotations

import logging
import threading
import time
from pathlib import Path

from swe_runner.agents import AgentAdapter, PreparedAgentRun
from swe_runner.common.models import AgentResult, InstanceResult, Settings, SWEInstance
from swe_runner.run.execution.complete import complete_instance_run
from swe_runner.run.io.artifacts import RunArtifacts, merge_metadata
from swe_runner.run.io.input_manifest import write_input_manifest
from swe_runner.run.io.output_store import RunOutputStore

logger = logging.getLogger(__name__)


class InstanceRunLifecycle:
    """Run one SWE-bench instance through prepare, invoke, finalize, and output persistence."""

    def __init__(
        self,
        agent: AgentAdapter,
        settings: Settings,
        output_dir: Path,
        *,
        output_store: RunOutputStore | None = None,
    ) -> None:
        self._agent = agent
        self._settings = settings
        self._output_dir = output_dir
        self._output_store = output_store or RunOutputStore(output_dir)

    def run(self, instance: SWEInstance) -> InstanceResult:
        """Execute the full lifecycle for one instance."""
        thread_name = threading.current_thread().name
        logger.info(
            "INSTANCE_START instance=%s thread=%s agent=%s",
            instance.instance_id,
            thread_name,
            self._agent.name,
        )

        prepared, prepare_error = self._prepare(instance)
        if prepared is None:
            return self._finish_failed_prepare(instance, prepare_error)

        manifest_error = self._write_input_manifest(prepared)
        if manifest_error is not None:
            return self._finish_failed_manifest(prepared, manifest_error)

        agent_result = self._invoke_agent(prepared, thread_name)
        agent_result = self._merge_prepared_metadata(prepared, agent_result)
        result = complete_instance_run(
            agent_name=self._agent.name,
            prepared=prepared,
            agent_result=agent_result,
            output_dir=self._output_dir,
        )
        self._log_end(result, thread_name)
        return result

    def _prepare(self, instance: SWEInstance) -> tuple[PreparedAgentRun | None, str]:
        try:
            return self._agent.prepare(instance, self._settings), ""
        except Exception as exc:
            logger.exception("AGENT_PREPARE_FAILED instance=%s agent=%s", instance.instance_id, self._agent.name)
            return None, f"Prepare error: {exc}"

    def _write_input_manifest(self, prepared: PreparedAgentRun) -> str | None:
        try:
            manifest_path = write_input_manifest(self._output_dir, agent_name=self._agent.name, prepared=prepared)
        except Exception as exc:
            logger.exception(
                "INPUT_MANIFEST_FAILED instance=%s agent=%s", prepared.instance.instance_id, self._agent.name
            )
            return f"Input manifest error: {exc}"

        prepared.metadata = (
            RunArtifacts.from_metadata(prepared.metadata)
            .with_updates(input_manifest_path=str(manifest_path))
            .to_metadata()
        )
        return None

    def _invoke_agent(self, prepared: PreparedAgentRun, thread_name: str) -> AgentResult:
        start_time = time.monotonic()
        try:
            logger.info(
                "AGENT_INVOKE instance=%s agent=%s thread=%s timeout=%ss step_limit=%s",
                prepared.instance.instance_id,
                self._agent.name,
                thread_name,
                prepared.timeout,
                prepared.max_turns,
            )
            agent_result = self._agent.run(prepared)
        except Exception as exc:
            duration = round(time.monotonic() - start_time, 2)
            logger.exception(
                "AGENT_FAILED instance=%s agent=%s thread=%s duration=%.2fs",
                prepared.instance.instance_id,
                self._agent.name,
                thread_name,
                duration,
            )
            return AgentResult(
                raw_output=str(exc),
                patch=None,
                success=False,
                duration_seconds=duration,
                error=str(exc),
                metadata=prepared.metadata,
            )

        logger.info(
            "AGENT_FINISH instance=%s agent=%s thread=%s success=%s duration=%.2fs",
            prepared.instance.instance_id,
            self._agent.name,
            thread_name,
            agent_result.success,
            agent_result.duration_seconds,
        )
        return agent_result

    @staticmethod
    def _merge_prepared_metadata(prepared: PreparedAgentRun, agent_result: AgentResult) -> AgentResult:
        if not prepared.metadata:
            return agent_result
        return agent_result.model_copy(
            update={"metadata": merge_metadata(prepared.metadata, agent_result.metadata)},
        )

    def _finish_failed_prepare(self, instance: SWEInstance, error: str) -> InstanceResult:
        result = _failed_result(instance, error)
        self._output_store.save_instance_result(result)
        return result

    def _finish_failed_manifest(self, prepared: PreparedAgentRun, error: str) -> InstanceResult:
        prepared.cleanup()
        result = _failed_result(prepared.instance, error)
        self._output_store.save_instance_result(result)
        return result

    def _log_end(self, result: InstanceResult, thread_name: str) -> None:
        logger.info(
            "INSTANCE_END instance=%s thread=%s success=%s has_patch=%s output_dir=%s",
            result.instance.instance_id,
            thread_name,
            result.success,
            result.agent_result.patch is not None,
            self._output_dir,
        )


def _failed_result(instance: SWEInstance, error: str, *, raw_output: str = "", duration: float = 0.0) -> InstanceResult:
    return InstanceResult(
        instance=instance,
        prediction=None,
        agent_result=AgentResult(
            raw_output=raw_output,
            patch=None,
            success=False,
            duration_seconds=duration,
            error=error,
        ),
        success=False,
    )
