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

"""Fixed-port sandbox container lifecycle for per-trial isolation.

Each trial gets a fresh container bound to a KNOWN host port so that:
- The MCP sandbox URL registered in the gateway config remains stable.
- File-system state injected by a prior trial cannot leak.
- env_snapshot collection is always from a clean container.

Usage (inside _execute_one):
    handle = start_sandbox_container(image, run_id, host_port)
    try:
        ...  # inject files, run agent, collect snapshot
    finally:
        stop_sandbox_container(handle)
"""

from __future__ import annotations

import time
from typing import Any

import httpx

from ._common import log


def start_sandbox_container(
    image: str,
    run_id: str,
    host_port: int,
    sandbox_port: int = 8080,
    mem_limit: str = "2g",
    cpu_limit: float = 2.0,
):
    """Start sandbox container bound to a KNOWN host port.

    Returns a ContainerHandle (from claw_eval.runner.sandbox_runner).
    """
    from claw_eval.runner.sandbox_runner import ContainerHandle

    import docker

    client = docker.from_env()
    container = client.containers.run(
        image=image,
        detach=True,
        name=f"claw-agent-{run_id}",
        mem_limit=mem_limit,
        nano_cpus=int(cpu_limit * 1e9),
        ports={f"{sandbox_port}/tcp": host_port},  # FIXED port
        labels={"app": "claw-eval", "role": "agent", "run_id": run_id},
    )
    sandbox_url = f"http://localhost:{host_port}"
    try:
        _wait_healthy(f"{sandbox_url}/health")
        _probe_exec(sandbox_url)
        log(f"  [sandbox] container {run_id} ready on :{host_port}")
        return ContainerHandle(
            container=container,
            host_port=host_port,
            run_id=run_id,
            sandbox_url=sandbox_url,
        )
    except BaseException:
        # The caller cannot release a container until its handle is returned.
        try:
            _release_container(container, run_id)
        except BaseException as error:
            log(f"  [WARNING] startup cleanup({run_id}): {error}")
        raise


def _release_container(container: Any, run_id: str) -> None:
    errors = []
    removed = False
    for name, action in (
        ("stop", lambda: container.stop(timeout=10)),
        ("remove", lambda: container.remove(force=True)),
    ):
        try:
            action()
            if name == "remove":
                removed = True
        except BaseException as error:
            errors.append((name, error))
    for name, error in errors:
        log(f"  [WARNING] {name}_sandbox_container({run_id}): {error}")
    if removed:
        log(f"  [sandbox] container {run_id} removed")
        # Let the fixed host port be released before another trial binds it.
        time.sleep(1)
    for _, error in errors:
        if not isinstance(error, Exception):
            raise error


def stop_sandbox_container(handle) -> None:
    """Stop and remove container, then wait for port release."""
    _release_container(handle.container, handle.run_id)


def _wait_healthy(url: str, timeout: float = 30.0):
    """Poll health endpoint until container is ready."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            r = httpx.get(url, timeout=3)
            if r.status_code < 500:
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise TimeoutError(f"Sandbox not healthy at {url} within {timeout}s")


def _probe_exec(sandbox_url: str, max_attempts: int = 5):
    """Verify the container can actually execute commands (not just serve HTTP).

    After /health returns 200 the HTTP listener is up, but internal processes
    (e.g. the shell executor) may still be initializing. Send a trivial Bash
    command and retry with exponential backoff until it succeeds.
    """
    endpoint = f"{sandbox_url}/exec"
    payload = {"command": "echo ok", "timeout_seconds": 5}
    backoff = [1, 2, 4, 4, 4]  # seconds between retries

    for attempt in range(max_attempts):
        try:
            r = httpx.post(endpoint, json=payload, timeout=10)
            if r.status_code == 200:
                body = r.json()
                if "ok" in body.get("stdout", ""):
                    return
        except Exception:
            pass
        if attempt < max_attempts - 1:
            wait = backoff[attempt]
            log(
                f"  [sandbox] probe /exec attempt {attempt + 1}/{max_attempts} "
                f"failed, retrying in {wait}s"
            )
            time.sleep(wait)

    raise TimeoutError(
        f"Sandbox at {sandbox_url} passed /health but /exec probe failed "
        f"after {max_attempts} attempts"
    )
