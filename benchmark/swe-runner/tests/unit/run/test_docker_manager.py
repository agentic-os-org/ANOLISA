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

"""Unit tests for DockerManager with mocked subprocess calls."""

from __future__ import annotations

import subprocess
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.common.commands import CommandResult
from swe_runner.run.workspace.docker import (
    DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS,
    DockerManager,
    _safe_docker_name,
    prepare_workspace_from_image,
)


@pytest.fixture
def manager(tmp_path: Path) -> DockerManager:
    """Create a DockerManager with a temporary work_dir."""
    return DockerManager(
        image_name="test-image:latest",
        work_dir=tmp_path / "workdir",
        container_timeout="2h",
        pull_timeout=30,
    )


def _mock_run_success(stdout: str = "fake-container-id\n", returncode: int = 0):
    """Create a mock command result that returns success."""
    return CommandResult(args=("docker",), stdout=stdout, stderr="", returncode=returncode)


class TestStart:
    def test_default_pull_timeout_is_1200_seconds(self):
        manager = DockerManager(image_name="test-image:latest", instance_id="django__django-1234")

        assert manager.pull_timeout == DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS
        assert manager.pull_timeout == 1200

    def test_default_work_dir_uses_instance_id(self):
        manager = DockerManager(image_name="test-image:latest", instance_id="django__django-1234")

        assert manager.work_dir == Path("/tmp/swebench_work_django__django-1234")

    def test_start_calls_docker_pull_run_cp(self, manager: DockerManager, tmp_path: Path):
        """verify start() calls: docker pull -> docker rm -f (stale) -> docker run -> docker cp"""
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            manager.start()

            calls = mock_run.call_args_list
            assert len(calls) == 4

            # First call: docker pull
            assert calls[0][0][0][:2] == ["docker", "pull"]
            assert calls[0][0][0][2] == "test-image:latest"

            # Second call: docker rm -f (stale container removal)
            assert calls[1][0][0] == ["docker", "rm", "-f", manager.container_name]

            # Third call: docker run
            assert calls[2][0][0][0] == "docker"
            assert calls[2][0][0][1] == "run"
            assert "-v" in calls[2][0][0]
            mount_index = calls[2][0][0].index("-v")
            assert calls[2][0][0][mount_index + 1] == f"{manager.work_dir}:{manager.work_dir}"
            assert "-w" in calls[2][0][0]
            workdir_index = calls[2][0][0].index("-w")
            assert calls[2][0][0][workdir_index + 1] == str(manager.work_dir)

            # Fourth call: docker cp
            assert calls[3][0][0][:2] == ["docker", "cp"]

    def test_start_pulls_from_registry_and_tags_local_name(self, tmp_path: Path):
        manager = DockerManager(
            image_name="swebench/sweb.eval.x86_64.test:latest",
            work_dir=tmp_path / "workdir",
            pull_registry="registry.example.com",
        )

        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            manager.start()

            calls = mock_run.call_args_list
            assert calls[0][0][0] == [
                "docker",
                "pull",
                "registry.example.com/swebench/sweb.eval.x86_64.test:latest",
            ]
            assert calls[1][0][0] == [
                "docker",
                "tag",
                "registry.example.com/swebench/sweb.eval.x86_64.test:latest",
                "swebench/sweb.eval.x86_64.test:latest",
            ]
            assert calls[2][0][0] == ["docker", "rm", "-f", manager.container_name]
            assert calls[3][0][0][0:2] == ["docker", "run"]
            assert calls[4][0][0][0:2] == ["docker", "cp"]

    def test_prepare_workspace_uses_1200_second_pull_timeout_by_default(self, tmp_path: Path):
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            prepare_workspace_from_image(
                "swebench/sweb.eval.x86_64.test:latest",
                instance_id="django__django-1234",
                work_dir=tmp_path / "workdir",
            )

            assert mock_run.call_args_list[0].kwargs["timeout"] == 1200

    def test_start_returns_work_dir(self, manager: DockerManager, tmp_path: Path):
        """verify start() returns the work_dir Path"""
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            result = manager.start()

            assert result == manager.work_dir
            assert isinstance(result, Path)

    def test_start_creates_work_dir(self, manager: DockerManager, tmp_path: Path):
        """verify work_dir is created"""
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            manager.start()

            assert manager.work_dir.exists()


class TestExecute:
    def test_execute_calls_docker_exec(self, manager: DockerManager):
        """verify execute() calls docker exec with correct args"""
        manager._container_id = "fake-container-id"

        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success(stdout="command output\n")

            manager.execute("echo hello")

            call_args = mock_run.call_args[0][0]
            assert call_args[:3] == ["docker", "exec", "-w"]
            assert call_args[3] == str(manager.work_dir)
            assert call_args[4] == "fake-container-id"
            assert call_args[5] == "bash"
            assert call_args[6] == "-c"
            assert call_args[7] == "echo hello"

    def test_execute_returns_dict(self, manager: DockerManager):
        """verify return format has output, returncode, exception_info"""
        manager._container_id = "fake-container-id"

        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success(stdout="ok\n")

            result = manager.execute("true")

            assert isinstance(result, dict)
            assert "output" in result
            assert "returncode" in result
            assert "exception_info" in result


class TestCleanup:
    def test_cleanup_stops_container(self, manager: DockerManager):
        """verify cleanup() calls docker stop"""
        manager._container_id = "fake-container-id"

        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            manager.cleanup()

            # First call should be docker stop
            stop_call = mock_run.call_args_list[0]
            assert stop_call[0][0][:3] == ["docker", "stop", "--time"]

    def test_cleanup_removes_work_dir(self, manager: DockerManager, tmp_path: Path):
        """verify cleanup() removes work directory"""
        manager._container_id = "fake-container-id"
        manager.work_dir.mkdir(parents=True, exist_ok=True)
        (manager.work_dir / "some_file.txt").write_text("data")

        with patch("swe_runner.run.workspace.docker.run_command"):
            manager.cleanup()

        assert not manager.work_dir.exists()


class TestContextManager:
    def test_context_manager(self, manager: DockerManager, tmp_path: Path):
        """verify DockerManager works as context manager (start on enter, cleanup on exit)"""
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            with manager as ctx:
                assert ctx is manager
                assert manager.container_id == "fake-container-id"

        # After exiting, cleanup should have been called
        assert manager.container_id is None


class TestContainerId:
    def test_container_id_after_start(self, manager: DockerManager, tmp_path: Path):
        """verify container_id is set after start()"""
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            assert manager.container_id is None
            manager.start()
            assert manager.container_id == "fake-container-id"

    def test_container_id_none_after_cleanup(self, manager: DockerManager):
        """verify container_id is None after cleanup()"""
        manager._container_id = "fake-container-id"

        with patch("swe_runner.run.workspace.docker.run_command"):
            manager.cleanup()

        assert manager.container_id is None


class TestStableContainerName:
    def test_name_derived_from_instance_id(self) -> None:
        """Container name is deterministic from instance_id."""
        m1 = DockerManager(image_name="img:1", instance_id="django__django-1234")
        m2 = DockerManager(image_name="img:1", instance_id="django__django-1234")
        assert m1.container_name == m2.container_name
        assert m1.container_name == "swe-django__django-1234"

    def test_different_instance_ids_produce_different_names(self) -> None:
        m1 = DockerManager(image_name="img:1", instance_id="django__django-1111")
        m2 = DockerManager(image_name="img:1", instance_id="django__django-2222")
        assert m1.container_name != m2.container_name

    def test_safe_docker_name_sanitizes(self) -> None:
        assert _safe_docker_name("django__django-1234") == "django__django-1234"
        assert _safe_docker_name("foo bar/baz") == "foo-bar-baz"
        assert _safe_docker_name("repo@sha256:abc") == "repo-sha256-abc"

    def test_safe_docker_name_leading_non_alnum(self) -> None:
        assert _safe_docker_name("__leading") == "s__leading"


class TestStaleContainerRemoval:
    def test_start_removes_stale_container(self, manager: DockerManager, tmp_path: Path) -> None:
        """start() proactively removes any stale container with the same name."""
        with patch("swe_runner.run.workspace.docker.run_command") as mock_run:
            mock_run.return_value = _mock_run_success()

            manager.start()

            # Second command call should be the stale removal (after pull, before run)
            stale_call = mock_run.call_args_list[1]
            assert stale_call[0][0] == ["docker", "rm", "-f", manager.container_name]

    def test_start_succeeds_even_if_stale_removal_fails(self, manager: DockerManager, tmp_path: Path) -> None:
        """If stale container removal fails (e.g. container doesn't exist), start() continues."""

        def _side_effect(*args, **kwargs):
            cmd = args[0] if args else kwargs.get("args", [])
            if cmd[:3] == ["docker", "rm", "-f"]:
                raise subprocess.TimeoutExpired(cmd, 30)
            return _mock_run_success()

        with patch("swe_runner.run.workspace.docker.run_command", side_effect=_side_effect):
            manager.start()

        assert manager.container_id == "fake-container-id"
