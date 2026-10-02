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

"""Docker container lifecycle manager."""

from __future__ import annotations

import contextlib
import logging
import shutil
import subprocess
from pathlib import Path
from typing import Any

from swe_runner.common.commands import run_command
from swe_runner.run.workspace.docker_refs import (
    build_pull_image_name,
    default_work_dir,
    safe_docker_name,
)
from swe_runner.run.workspace.repo_excludes import install_repo_exclude_rules

logger = logging.getLogger(__name__)

DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS = 1200

__all__ = [
    "DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS",
    "DockerManager",
    "prepare_workspace_from_image",
    "pull_docker_image",
]


def _safe_docker_name(name: str) -> str:
    """Sanitize *name* so it is a valid Docker container name.

    Docker container names must match ``[a-zA-Z0-9][a-zA-Z0-9_.-]*``.
    """
    return safe_docker_name(name)


def _default_work_dir(instance_id: str) -> Path:
    return default_work_dir(instance_id)


def pull_docker_image(
    image_name: str,
    *,
    pull_registry: str | None = None,
    pull_timeout: int = DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS,
) -> None:
    """Pull an image, optionally via an alternate registry, and retag it to *image_name*."""
    pull_image_name = build_pull_image_name(image_name, pull_registry)
    run_command(
        ["docker", "pull", pull_image_name],
        timeout=pull_timeout,
    )
    if pull_image_name != image_name:
        run_command(
            ["docker", "tag", pull_image_name, image_name],
            timeout=30,
        )


def prepare_workspace_from_image(
    image_name: str,
    *,
    instance_id: str,
    work_dir: Path | None = None,
    pull_timeout: int = DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS,
    pull_registry: str | None = None,
) -> Path:
    """Copy `/testbed` from a SWE-bench image into a host work directory."""
    target_dir = work_dir or _default_work_dir(instance_id)
    container_name = f"swe-prep-{_safe_docker_name(instance_id)}"

    pull_docker_image(
        image_name,
        pull_registry=pull_registry,
        pull_timeout=pull_timeout,
    )
    if target_dir.exists():
        shutil.rmtree(target_dir, ignore_errors=True)
    target_dir.mkdir(parents=True, exist_ok=True)
    run_command(["docker", "rm", "-f", container_name], timeout=30)
    try:
        run_command(
            ["docker", "create", "--name", container_name, image_name, "sleep", "1"],
            check=True,
        )
        run_command(
            ["docker", "cp", f"{container_name}:/testbed/.", str(target_dir)],
            check=True,
        )
    finally:
        run_command(["docker", "rm", "-f", container_name], timeout=30)
    install_repo_exclude_rules(target_dir, instance_id=instance_id)
    return target_dir


class DockerManager:
    """Manages a Docker container for running SWE-bench tasks.

    Handles pulling images, starting/stopping containers, copying /testbed
    content to the host, and executing commands inside the container.
    """

    def __init__(
        self,
        image_name: str,
        *,
        instance_id: str = "unknown",
        work_dir: Path | None = None,
        container_timeout: str = "2h",
        pull_timeout: int = DEFAULT_DOCKER_PULL_TIMEOUT_SECONDS,
        pull_registry: str | None = None,
    ) -> None:
        self.image_name = image_name
        self.instance_id = instance_id
        self.work_dir = work_dir or _default_work_dir(instance_id)
        self.container_timeout = container_timeout
        self.pull_timeout = pull_timeout
        self.pull_registry = pull_registry
        self._container_id: str | None = None
        self._container_name = f"swe-{_safe_docker_name(instance_id)}"

    @property
    def container_id(self) -> str | None:
        """Return the running container ID, or None if not started."""
        return self._container_id

    @property
    def container_name(self) -> str:
        """Return the container name."""
        return self._container_name

    def _remove_stale_container(self) -> None:
        """Remove any existing container with our name so reruns don't conflict."""
        try:
            run_command(
                ["docker", "rm", "-f", self.container_name],
                timeout=30,
            )
        except Exception:
            logger.debug(
                "DOCKER_REMOVE_STALE_SKIP instance=%s container=%s",
                self.instance_id,
                self.container_name,
            )

    def start(self) -> Path:
        """Pull image, start container, copy /testbed to work_dir, return work_dir."""
        logger.info(
            "DOCKER_START instance=%s image=%s pull_registry=%s container=%s work_dir=%s pull_timeout=%ss container_timeout=%s",
            self.instance_id,
            self.image_name,
            self.pull_registry,
            self.container_name,
            self.work_dir,
            self.pull_timeout,
            self.container_timeout,
        )
        pull_docker_image(
            self.image_name,
            pull_registry=self.pull_registry,
            pull_timeout=self.pull_timeout,
        )
        logger.info("DOCKER_PULL_DONE instance=%s image=%s", self.instance_id, self.image_name)

        self.work_dir.mkdir(parents=True, exist_ok=True)

        # Remove any stale container with the same name so reruns don't clash
        self._remove_stale_container()

        result = run_command(
            [
                "docker",
                "run",
                "-d",
                "--name",
                self.container_name,
                "-v",
                f"{self.work_dir}:{self.work_dir}",
                "-w",
                str(self.work_dir),
                "--rm",
                self.image_name,
                "sleep",
                self.container_timeout,
            ],
            check=True,
        )

        self._container_id = result.stdout.strip()
        logger.info(
            "DOCKER_RUN_DONE instance=%s container=%s container_id=%s",
            self.instance_id,
            self.container_name,
            self._container_id,
        )

        run_command(
            ["docker", "cp", f"{self._container_id}:/testbed/.", str(self.work_dir)],
            check=True,
        )
        logger.info(
            "DOCKER_COPY_DONE instance=%s container=%s work_dir=%s",
            self.instance_id,
            self.container_name,
            self.work_dir,
        )

        install_repo_exclude_rules(self.work_dir, instance_id=self.instance_id)

        return self.work_dir

    def execute(self, command: str, timeout: int = 60) -> dict[str, Any]:
        """Execute command inside container in the mounted work_dir.

        Returns a dict with keys: output, returncode, exception_info.
        """
        if self._container_id is None:
            raise RuntimeError("Container not started")

        logger.info(
            "DOCKER_EXEC_START instance=%s container=%s container_id=%s timeout=%ss command=%s",
            self.instance_id,
            self.container_name,
            self._container_id,
            timeout,
            command,
        )
        try:
            result = run_command(
                [
                    "docker",
                    "exec",
                    "-w",
                    str(self.work_dir),
                    self._container_id,
                    "bash",
                    "-c",
                    command,
                ],
                timeout=timeout,
            )
            logger.info(
                "DOCKER_EXEC_END instance=%s container=%s returncode=%s stdout_bytes=%s stderr_bytes=%s",
                self.instance_id,
                self.container_name,
                result.returncode,
                len(result.stdout),
                len(result.stderr),
            )
            return {
                "output": result.stdout,
                "returncode": result.returncode,
                "exception_info": result.stderr,
            }
        except subprocess.TimeoutExpired:
            logger.warning(
                "DOCKER_EXEC_TIMEOUT instance=%s container=%s timeout=%ss",
                self.instance_id,
                self.container_name,
                timeout,
            )
            return {
                "output": "",
                "returncode": -1,
                "exception_info": f"Command timed out after {timeout}s",
            }
        except Exception as exc:
            logger.exception("DOCKER_EXEC_ERROR instance=%s container=%s", self.instance_id, self.container_name)
            return {
                "output": "",
                "returncode": -1,
                "exception_info": str(exc),
            }

    def cleanup(self, timeout: int = 60) -> None:
        """Stop container, remove work_dir."""
        logger.info(
            "DOCKER_CLEANUP_START instance=%s container=%s container_id=%s work_dir=%s timeout=%ss",
            self.instance_id,
            self.container_name,
            self._container_id,
            self.work_dir,
            timeout,
        )
        try:
            run_command(
                ["docker", "stop", "--time", str(timeout), self.container_name],
                timeout=timeout + 30,
            )
            logger.info("DOCKER_STOP_DONE instance=%s container=%s", self.instance_id, self.container_name)
        except Exception:
            logger.warning(
                "DOCKER_STOP_FAILED instance=%s container=%s fallback=rm -f",
                self.instance_id,
                self.container_name,
            )
            with contextlib.suppress(Exception):
                run_command(
                    ["docker", "rm", "-f", self.container_name],
                    timeout=30,
                )
                logger.info("DOCKER_REMOVE_DONE instance=%s container=%s", self.instance_id, self.container_name)

        self._container_id = None

        if self.work_dir.exists():
            shutil.rmtree(self.work_dir, ignore_errors=True)
            logger.info("DOCKER_WORKDIR_REMOVED instance=%s work_dir=%s", self.instance_id, self.work_dir)

    def __enter__(self) -> DockerManager:
        self.start()
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: Any,
    ) -> None:
        self.cleanup()
