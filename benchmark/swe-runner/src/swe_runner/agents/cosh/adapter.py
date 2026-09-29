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

"""Adapter for the cosh CLI tool."""

import logging
import subprocess
import threading
import time

from swe_runner.agents import (
    AgentAdapter,
    AgentNotFoundError,
    AgentStepLimitError,
    AgentTimeoutError,
    PreparedAgentRun,
    register_agent,
)
from swe_runner.common.commands import run_command
from swe_runner.common.models import AgentResult, Settings, SWEInstance
from swe_runner.run.io.artifacts import RunArtifacts
from swe_runner.run.prompting.prompts import build_prompt
from swe_runner.run.workspace.docker import DockerManager
from swe_runner.run.workspace.docker_refs import get_docker_image_name
from swe_runner.run.workspace.git import get_git_revision

logger = logging.getLogger(__name__)

STEP_LIMIT_EXIT_CODE = 53


class CoshAdapter(AgentAdapter):
    """Adapter for the cosh CLI tool."""

    def __init__(self, cosh_path: str = "cosh") -> None:
        self._cosh_path = cosh_path

    @property
    def name(self) -> str:
        return "cosh"

    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        image_name = get_docker_image_name(instance)
        docker = DockerManager(
            image_name,
            instance_id=instance.instance_id,
            pull_registry=settings.agent.docker_pull_registry,
        )
        work_dir = docker.start()
        base_revision = get_git_revision(work_dir)
        prompt = build_prompt(
            instance,
            work_dir,
            docker.container_name,
            agent_name=self.name,
            use_skill=settings.agent.use_skill,
            skills_dir=settings.agent.skills_dir,
            use_per_case_prompt=settings.agent.per_case_prompt,
            prompts_dir=settings.agent.prompts_dir,
        )
        return PreparedAgentRun(
            instance=instance,
            settings=settings,
            work_dir=work_dir,
            prompt=prompt,
            timeout=settings.agent.timeout,
            max_turns=settings.agent.step_limit,
            base_revision=base_revision,
            metadata=RunArtifacts(docker_image_name=image_name).to_metadata(),
            cleanup_callbacks=[docker.cleanup],
        )

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        prompt = prepared.prompt
        work_dir = prepared.work_dir
        timeout = prepared.timeout
        max_turns = prepared.max_turns
        instance_id = prepared.instance.instance_id
        thread_name = threading.current_thread().name

        cmd = [self._cosh_path, "--yolo", prompt]
        if max_turns > 0:
            cmd.extend(["--max-session-turns", str(max_turns)])

        logger.info(
            "COSH_AGENT_START instance=%s thread=%s cwd=%s timeout=%ss max_turns=%s",
            instance_id,
            thread_name,
            work_dir,
            timeout,
            max_turns,
        )
        logger.debug("COSH_AGENT_COMMAND instance=%s cmd=%s", instance_id, " ".join(cmd))

        start_time = time.monotonic()
        try:
            result = run_command(
                cmd,
                cwd=work_dir,
                timeout=timeout,
                encoding="utf-8",
                errors="replace",
            )
        except subprocess.TimeoutExpired:
            raise AgentTimeoutError(f"Agent timed out after {timeout}s") from None
        except FileNotFoundError:
            raise AgentNotFoundError(f"'{self._cosh_path}' not found in PATH") from None

        duration = time.monotonic() - start_time
        raw_output = result.output
        logger.info(
            "COSH_AGENT_END instance=%s thread=%s returncode=%s duration=%.2fs",
            instance_id,
            thread_name,
            result.returncode,
            duration,
        )

        if result.returncode == STEP_LIMIT_EXIT_CODE:
            raise AgentStepLimitError(f"Agent reached step limit (exit code {STEP_LIMIT_EXIT_CODE})")

        return AgentResult(
            raw_output=raw_output,
            patch=None,  # Core extracts patches
            success=result.returncode == 0,
            duration_seconds=round(duration, 2),
        )


register_agent("cosh", CoshAdapter)
