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

"""A missing health URL does not mean a declared process already exists."""

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import pytest
import yaml
from ce_runner import parallel
from ce_runner.mcp_mock_services import MockServiceManager


def _task(tmp_path: Path, health: str | None) -> Path:
    service = {
        "name": "contacts",
        "port": 9103,
        "command": "python service.py",
        "ready_timeout": 0,
    }
    if health is not None:
        service["health_check"] = health
    path = tmp_path / "tasks" / "T001" / "task.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(yaml.safe_dump({"services": [service]}), encoding="utf-8")
    return path


@pytest.mark.parametrize("launcher", ["parallel", "mcp"])
@pytest.mark.parametrize("health", [None, ""])
def test_services_without_health_urls_start_without_probing(
    tmp_path: Path, launcher: str, health: str | None
) -> None:
    path = _task(tmp_path, health)
    with (
        patch("subprocess.Popen") as popen,
        patch("httpx.post", side_effect=AssertionError("no empty URL probe")) as post,
        patch("httpx.get", side_effect=AssertionError("no empty URL probe")) as get,
        patch("time.sleep") as sleep,
    ):
        if launcher == "parallel":
            parallel.start_mock_services_with_offset(str(path), str(path.parent), 50)
        else:
            manager = MockServiceManager(path, port_offset=50)
            manager.start_all()
            assert manager.processes == [popen.return_value]

    popen.assert_called_once()
    assert popen.call_args.kwargs["env"]["PORT"] == "9153"
    post.assert_not_called()
    get.assert_not_called()
    sleep.assert_not_called()


@pytest.mark.parametrize("launcher", ["parallel", "mcp"])
def test_declared_healthy_services_are_reused(tmp_path: Path, launcher: str) -> None:
    path = _task(tmp_path, "http://localhost:9103/health")
    with (
        patch("subprocess.Popen") as popen,
        patch("httpx.post", return_value=SimpleNamespace(status_code=200)) as post,
    ):
        if launcher == "parallel":
            parallel.start_mock_services_with_offset(str(path), str(path.parent), 50)
        else:
            MockServiceManager(path, port_offset=50).start_all()

    popen.assert_not_called()
    assert post.call_args.args[0] == "http://localhost:9153/health"


@pytest.mark.parametrize("launcher", ["parallel", "mcp"])
def test_unhealthy_declared_services_still_wait_for_readiness(
    tmp_path: Path, launcher: str
) -> None:
    path = _task(tmp_path, "http://localhost:9103/health")
    task = yaml.safe_load(path.read_text(encoding="utf-8"))
    task["services"][0]["ready_timeout"] = 1
    path.write_text(yaml.safe_dump(task), encoding="utf-8")
    with (
        patch("subprocess.Popen") as popen,
        patch(
            "httpx.post",
            side_effect=[
                SimpleNamespace(status_code=503),
                SimpleNamespace(status_code=200),
            ],
        ) as post,
        patch("time.sleep"),
    ):
        if launcher == "parallel":
            parallel.start_mock_services_with_offset(str(path), str(path.parent), 50)
        else:
            MockServiceManager(path, port_offset=50).start_all()

    popen.assert_called_once()
    assert post.call_count == 2
    assert all(
        call.args[0] == "http://localhost:9153/health" for call in post.call_args_list
    )
