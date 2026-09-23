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

"""Environment pre-flight checks for agents."""

from __future__ import annotations

import logging
import subprocess
from typing import Any

from swe_runner.agents import AgentEnvironmentError
from swe_runner.agents.registry import iter_environment_checks, iter_required_binaries
from swe_runner.common.commands import run_command

logger = logging.getLogger(__name__)

def check_docker_available() -> None:
    """Check if Docker daemon is accessible.

    Raises:
        AgentEnvironmentError: If Docker is not available
    """
    try:
        result = run_command(
            ["docker", "info"],
            timeout=5,
        )
        if result.returncode != 0:
            raise AgentEnvironmentError(
                f"Docker daemon is not accessible. Error: {result.stderr.strip() or 'Docker may not be running'}"
            )
    except FileNotFoundError as err:
        raise AgentEnvironmentError(
            "Docker command not found. Please install Docker: https://docs.docker.com/get-docker/"
        ) from err
    except subprocess.TimeoutExpired as err:
        raise AgentEnvironmentError("Docker daemon check timed out. Docker may be unresponsive.") from err


def check_disk_space(min_gb: float = 10.0, path: str = "/") -> None:
    """Check if sufficient disk space is available.

    Args:
        min_gb: Minimum required free space in GB (default: 10GB)
        path: Path to check disk space for

    Raises:
        AgentEnvironmentError: If insufficient disk space
    """
    import shutil

    try:
        usage = shutil.disk_usage(path)
        free_gb = usage.free / (1024**3)
        if free_gb < min_gb:
            raise AgentEnvironmentError(
                f"Insufficient disk space: {free_gb:.1f}GB available, {min_gb}GB required. "
                f"Free up disk space or specify a different output directory."
            )
    except Exception as e:
        if isinstance(e, AgentEnvironmentError):
            raise
        raise AgentEnvironmentError(f"Failed to check disk space: {e}") from e


def check_agent_binary(agent_name: str) -> None:
    """Check if agent binary is available in PATH.

    Currently supports agents that require a local CLI binary.

    Args:
        agent_name: Name of the agent

    Raises:
        AgentEnvironmentError: If required binary is not found
    """
    import shutil

    for binary in iter_required_binaries(agent_name):
        if shutil.which(binary) is None:
            raise AgentEnvironmentError(
                f"Agent binary '{binary}' not found in PATH. Please install {agent_name} agent or add it to your PATH."
            )


def check_agent_environment(agent_name: str, *, include_agent_specific: bool = True, **kwargs: Any) -> None:
    """Run environment pre-flight checks for the specified agent.

    Performs the following checks in order:
    1. Docker daemon availability (all agents)
    2. Disk space availability (all agents)
    3. Agent binary availability
    4. Agent-specific environment checks, when registered

    Args:
        agent_name: Name of the agent to check
        **kwargs: Additional arguments passed to agent-specific checks

    Raises:
        AgentEnvironmentError: If any environment check fails
    """
    logger.info("ENV_CHECK_START agent=%s", agent_name)

    try:
        # Universal checks (all agents)
        logger.debug("Checking Docker availability")
        check_docker_available()

        logger.debug("Checking disk space")
        check_disk_space()

        # Agent binary check
        logger.debug("Checking agent binary for %s", agent_name)
        check_agent_binary(agent_name)

        if include_agent_specific:
            check_fns = iter_environment_checks(agent_name)
            if check_fns:
                logger.debug("Running agent-specific checks for %s", agent_name)
                for check_fn in check_fns:
                    check_fn(**kwargs)

        logger.info("ENV_CHECK_PASS agent=%s", agent_name)

    except AgentEnvironmentError:
        logger.error("ENV_CHECK_FAIL agent=%s", agent_name)
        raise
    except Exception as e:
        logger.error("ENV_CHECK_FAIL agent=%s error=%s", agent_name, e)
        raise AgentEnvironmentError(f"Environment check failed: {e}") from e
