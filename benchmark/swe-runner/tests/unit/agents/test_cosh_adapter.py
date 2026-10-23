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

"""Unit tests for CoshAdapter adapter."""

from __future__ import annotations

import subprocess
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from swe_runner.agents import AgentNotFoundError, AgentStepLimitError, AgentTimeoutError, get_agent
from swe_runner.agents.cosh.adapter import STEP_LIMIT_EXIT_CODE, CoshAdapter
from swe_runner.agents.lifecycle import PreparedAgentRun
from swe_runner.common.commands import CommandResult
from swe_runner.common.models import AgentConfig, AgentResult, Settings, SWEInstance


@pytest.fixture
def agent() -> CoshAdapter:
    return CoshAdapter()


def make_prepared(*, max_turns: int = 0) -> PreparedAgentRun:
    instance = SWEInstance(
        instance_id="test-instance",
        repo="example/repo",
        version="1",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )
    settings = Settings(agent=AgentConfig(name="cosh", timeout=1800, step_limit=max_turns))
    return PreparedAgentRun(
        instance=instance,
        settings=settings,
        work_dir=Path("/tmp/workdir"),
        prompt="fix the bug",
        timeout=1800,
        max_turns=max_turns,
    )


# -- Preparation failure cleanup --


def make_instance_and_settings() -> tuple[SWEInstance, Settings]:
    instance = SWEInstance(
        instance_id="test-instance",
        repo="example/repo",
        version="1",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )
    settings = Settings(agent=AgentConfig(name="cosh", timeout=1800, step_limit=0))
    return instance, settings


def _prepare_with_docker(agent: CoshAdapter, docker: MagicMock, prompt_side_effect: Exception | None = None) -> None:
    with (
        patch("swe_runner.agents.cosh.adapter.get_docker_image_name", return_value="img"),
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        patch("swe_runner.agents.cosh.adapter.get_git_revision", return_value="abc123"),
        patch(
            "swe_runner.agents.cosh.adapter.build_prompt",
            side_effect=prompt_side_effect,
        ),
    ):
        instance, settings = make_instance_and_settings()
        agent.prepare(instance, settings)


def test_prepare_cleans_up_container_when_prompt_fails(agent: CoshAdapter) -> None:
    docker = MagicMock()
    docker.start.return_value = Path("/tmp/workdir")
    with pytest.raises(UnicodeDecodeError):
        _prepare_with_docker(
            agent, docker, prompt_side_effect=UnicodeDecodeError("utf-8", b"", 0, 1, "bad prompt bytes")
        )
    docker.cleanup.assert_called_once()


def test_prepare_cleans_up_container_when_revision_probe_fails(agent: CoshAdapter) -> None:
    docker = MagicMock()
    docker.start.return_value = Path("/tmp/workdir")
    with patch("swe_runner.agents.cosh.adapter.get_docker_image_name", return_value="img"),          patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),          patch("swe_runner.agents.cosh.adapter.get_git_revision", side_effect=RuntimeError("git gone")):
        instance, settings = make_instance_and_settings()
        with pytest.raises(RuntimeError):
            agent.prepare(instance, settings)
    docker.cleanup.assert_called_once()


def test_prepare_registers_cleanup_callback_on_success(agent: CoshAdapter) -> None:
    docker = MagicMock()
    docker.start.return_value = Path("/tmp/workdir")
    _prepare_with_docker(agent, docker)
    docker.cleanup.assert_not_called()


# -- Basic property test --


def test_cosh_adapter_name(agent: CoshAdapter) -> None:
    assert agent.name == "cosh"


# -- Success case --


@patch("swe_runner.agents.cosh.adapter.run_command")
def test_cosh_adapter_run_success(mock_run: MagicMock, agent: CoshAdapter) -> None:
    mock_run.return_value = CommandResult(
        args=("cosh",),
        returncode=0,
        stdout="done output\n",
        stderr="",
    )

    result = agent.run(make_prepared())

    assert isinstance(result, AgentResult)
    assert result.success is True
    assert "done output" in result.raw_output
    assert result.patch is None
    assert result.duration_seconds >= 0

    mock_run.assert_called_once()
    call_args = mock_run.call_args
    assert call_args.kwargs["cwd"] == Path("/tmp/workdir")


# -- max_turns flag --


@patch("swe_runner.agents.cosh.adapter.run_command")
def test_cosh_adapter_run_with_max_turns(mock_run: MagicMock, agent: CoshAdapter) -> None:
    mock_run.return_value = CommandResult(args=("cosh",), stdout="", stderr="", returncode=0)

    agent.run(make_prepared(max_turns=10))

    call_args = mock_run.call_args
    cmd = call_args.args[0]
    assert "--max-session-turns" in cmd
    assert "10" in cmd


@patch("swe_runner.agents.cosh.adapter.run_command")
def test_cosh_adapter_run_without_max_turns(mock_run: MagicMock, agent: CoshAdapter) -> None:
    mock_run.return_value = CommandResult(args=("cosh",), stdout="", stderr="", returncode=0)

    agent.run(make_prepared())

    call_args = mock_run.call_args
    cmd = call_args.args[0]
    assert "--max-session-turns" not in cmd


# -- Timeout --


@patch("swe_runner.agents.cosh.adapter.run_command", side_effect=subprocess.TimeoutExpired(cmd="cosh", timeout=1800))
def test_cosh_adapter_timeout(mock_run: MagicMock, agent: CoshAdapter) -> None:
    with pytest.raises(AgentTimeoutError, match="timed out"):
        agent.run(make_prepared())


# -- Binary not found --


@patch("swe_runner.agents.cosh.adapter.run_command", side_effect=FileNotFoundError("cosh not found"))
def test_cosh_adapter_not_found(mock_run: MagicMock, agent: CoshAdapter) -> None:
    with pytest.raises(AgentNotFoundError, match="not found"):
        agent.run(make_prepared())


# -- Step limit --


@patch("swe_runner.agents.cosh.adapter.run_command")
def test_cosh_adapter_step_limit(mock_run: MagicMock, agent: CoshAdapter) -> None:
    mock_run.return_value = CommandResult(args=("cosh",), stdout="", stderr="", returncode=STEP_LIMIT_EXIT_CODE)

    with pytest.raises(AgentStepLimitError, match="step limit"):
        agent.run(make_prepared())


# -- Non-zero exit code --


@patch("swe_runner.agents.cosh.adapter.run_command")
def test_cosh_adapter_nonzero_exit(mock_run: MagicMock, agent: CoshAdapter) -> None:
    mock_run.return_value = CommandResult(args=("cosh",), stdout="some output", stderr="error msg", returncode=1)

    result = agent.run(make_prepared())

    assert isinstance(result, AgentResult)
    assert result.success is False
    assert "some output" in result.raw_output
    assert "error msg" in result.raw_output


# -- Registry --


def test_cosh_adapter_registered() -> None:
    agent_instance = get_agent("cosh")
    assert isinstance(agent_instance, CoshAdapter)
