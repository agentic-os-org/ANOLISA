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

"""Preparation resource ownership before the cosh run is handed to its caller."""

from __future__ import annotations

import shutil
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from swe_runner.agents.cosh.adapter import CoshAdapter
from swe_runner.common.models import AgentConfig, Settings, SWEInstance


def inputs() -> tuple[SWEInstance, Settings]:
    instance = SWEInstance(
        instance_id="fixture-cosh-prepare",
        repo="example/repo",
        version="1",
        base_commit="fixture-revision",
        problem_statement="Fixture task",
        patch="",
        test_patch="",
    )
    return instance, Settings(agent=AgentConfig(name="cosh", timeout=123, step_limit=7))


@pytest.mark.parametrize("boundary", ["get_git_revision", "build_prompt", "metadata", "PreparedAgentRun"])
@pytest.mark.parametrize("error_type", [RuntimeError, KeyboardInterrupt])
def test_prepare_rolls_back_each_post_start_boundary(
    tmp_path: Path, boundary: str, error_type: type[BaseException]
) -> None:
    work_dir = tmp_path / "work"
    work_dir.mkdir()
    (work_dir / "owned.txt").write_text("owned", encoding="utf-8")
    docker = MagicMock()
    docker.start.return_value = work_dir

    def cleanup(*, timeout: int = 60) -> None:
        assert timeout == 0
        assert work_dir.parent == tmp_path
        shutil.rmtree(work_dir)

    docker.cleanup.side_effect = cleanup
    error = error_type("controlled preparation failure")
    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        patch("swe_runner.agents.cosh.adapter.get_git_revision", return_value="fixture-revision") as revision,
        patch("swe_runner.agents.cosh.adapter.build_prompt", return_value="fixture prompt") as prompt,
        patch("swe_runner.agents.cosh.adapter.RunArtifacts.to_metadata", return_value={}) as metadata,
        patch("swe_runner.agents.cosh.adapter.PreparedAgentRun") as prepared,
    ):
        {"get_git_revision": revision, "build_prompt": prompt, "metadata": metadata, "PreparedAgentRun": prepared}[
            boundary
        ].side_effect = error
        with pytest.raises(error_type) as caught:
            CoshAdapter().prepare(*inputs())

    assert caught.value is error
    docker.cleanup.assert_called_once_with(timeout=0)
    assert not work_dir.exists()


@pytest.mark.parametrize("error_type", [RuntimeError, KeyboardInterrupt])
def test_artifacts_constructor_failure_also_rolls_back(tmp_path: Path, error_type: type[BaseException]) -> None:
    docker = MagicMock()
    docker.start.return_value = tmp_path
    error = error_type("controlled artifacts constructor failure")
    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        patch("swe_runner.agents.cosh.adapter.get_git_revision", return_value="fixture-revision"),
        patch("swe_runner.agents.cosh.adapter.build_prompt", return_value="fixture prompt"),
        patch("swe_runner.agents.cosh.adapter.RunArtifacts", side_effect=error),
        pytest.raises(error_type) as caught,
    ):
        CoshAdapter().prepare(*inputs())

    assert caught.value is error
    docker.cleanup.assert_called_once_with(timeout=0)


@pytest.mark.parametrize("preparation_error", [RuntimeError, KeyboardInterrupt])
@pytest.mark.parametrize("cleanup_error", [RuntimeError, KeyboardInterrupt, SystemExit])
def test_rollback_failure_never_replaces_the_preparation_failure(
    tmp_path: Path, preparation_error: type[BaseException], cleanup_error: type[BaseException]
) -> None:
    docker = MagicMock()
    docker.start.return_value = tmp_path
    docker.cleanup.side_effect = cleanup_error("controlled cleanup failure")
    error = preparation_error("original preparation failure")
    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        patch("swe_runner.agents.cosh.adapter.get_git_revision", side_effect=error),
        pytest.raises(preparation_error) as caught,
    ):
        CoshAdapter().prepare(*inputs())

    assert caught.value is error
    docker.cleanup.assert_called_once_with(timeout=0)


def test_success_hands_off_cleanup_once_without_early_rollback(tmp_path: Path) -> None:
    work_dir = tmp_path / "work"
    work_dir.mkdir()
    docker = MagicMock()
    docker.start.return_value = work_dir

    def cleanup() -> None:
        assert work_dir.parent == tmp_path
        shutil.rmtree(work_dir)

    docker.cleanup.side_effect = cleanup
    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        patch("swe_runner.agents.cosh.adapter.get_git_revision", return_value="fixture-revision"),
        patch("swe_runner.agents.cosh.adapter.build_prompt", return_value="fixture prompt"),
        patch("swe_runner.agents.cosh.adapter.RunArtifacts.to_metadata", return_value={"fixture": "metadata"}),
    ):
        prepared = CoshAdapter().prepare(*inputs())

    assert prepared.work_dir == work_dir
    assert prepared.prompt == "fixture prompt"
    assert prepared.base_revision == "fixture-revision"
    assert prepared.timeout == 123
    assert prepared.max_turns == 7
    assert prepared.metadata == {"fixture": "metadata"}
    docker.cleanup.assert_not_called()
    assert work_dir.exists()
    prepared.cleanup()
    prepared.cleanup()
    docker.cleanup.assert_called_once_with()
    assert not work_dir.exists()


def test_start_failure_remains_owned_by_the_docker_manager() -> None:
    docker = MagicMock()
    error = KeyboardInterrupt("controlled start interruption")
    docker.start.side_effect = error
    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        pytest.raises(KeyboardInterrupt) as caught,
    ):
        CoshAdapter().prepare(*inputs())

    assert caught.value is error
    docker.cleanup.assert_not_called()


def test_rollback_logging_failure_cannot_replace_the_original_error(tmp_path: Path) -> None:
    docker = MagicMock()
    docker.start.return_value = tmp_path
    docker.cleanup.side_effect = RuntimeError("controlled cleanup failure")
    primary = RuntimeError("original preparation failure")
    with (
        patch("swe_runner.agents.cosh.adapter.DockerManager", return_value=docker),
        patch("swe_runner.agents.cosh.adapter.get_git_revision", side_effect=primary),
        patch("swe_runner.agents.cosh.adapter.logger.exception", side_effect=KeyboardInterrupt("logging interrupted")),
        pytest.raises(BaseException) as caught,
    ):
        CoshAdapter().prepare(*inputs())

    assert caught.value is primary
    docker.cleanup.assert_called_once_with(timeout=0)
