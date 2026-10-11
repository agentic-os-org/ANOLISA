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

"""Shift only explicit loopback URL ports consistently across service clients."""

import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import pytest
import yaml
from ce_runner import parallel
from ce_runner.mcp_mock_services import MockServiceManager, _shift_url


@pytest.mark.parametrize("host", ["localhost", "127.0.0.1", "[::1]"])
def test_loopback_ports_shift_without_altering_other_url_parts(host: str) -> None:
    url = (
        f"http://user:pass@{host}:9103/api/localhost:9103?next=localhost:9103#fragment"
    )
    assert (
        _shift_url(url, 50)
        == f"http://user:pass@{host}:9153/api/localhost:9103?next=localhost:9103#fragment"
    )
    assert _shift_url(url, 0) == url


@pytest.mark.parametrize(
    "url",
    [
        "",
        "http://localhost/health",
        "https://example.com:9103/localhost:9103",
        "http://example.com/path?next=http://localhost:9103/",
    ],
)
def test_nonloopback_authorities_and_missing_ports_are_preserved(url: str) -> None:
    assert _shift_url(url, 50) == url


def _task(tmp_path: Path, host: str) -> Path:
    service = {
        "name": "contacts",
        "port": 9103,
        "command": "python service.py",
        "health_check": f"http://{host}:9103/health",
        "reset_endpoint": f"http://{host}:9103/reset",
        "ready_timeout": 0,
    }
    task = {
        "services": [service],
        "tools": [{"name": "search"}],
        "tool_endpoints": [
            {"tool_name": "search", "url": f"http://{host}:9103/search"}
        ],
    }
    path = tmp_path / "tasks" / "T001" / "task.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(yaml.safe_dump(task), encoding="utf-8")
    return path


@pytest.mark.parametrize("host", ["localhost", "127.0.0.1", "[::1]"])
def test_parallel_health_and_reset_use_shifted_port(tmp_path: Path, host: str) -> None:
    path = _task(tmp_path, host)
    with (
        patch("httpx.post", return_value=SimpleNamespace(status_code=200)) as post,
        patch("subprocess.Popen") as popen,
    ):
        parallel.start_mock_services_with_offset(str(path), str(path.parent), 50)
        parallel.reset_services_with_offset(str(path), 50)
    assert [call.args[0] for call in post.call_args_list] == [
        f"http://{host}:9153/health",
        f"http://{host}:9153/reset",
    ]
    popen.assert_not_called()


@pytest.mark.parametrize("host", ["localhost", "127.0.0.1", "[::1]"])
def test_mcp_health_reset_and_tools_use_shifted_port(tmp_path: Path, host: str) -> None:
    manager = MockServiceManager(_task(tmp_path, host), port_offset=50)
    with (
        patch("httpx.post", return_value=SimpleNamespace(status_code=200)) as post,
        patch("subprocess.Popen") as popen,
    ):
        manager.start_all()
        manager.reset_all()
    assert [call.args[0] for call in post.call_args_list] == [
        f"http://{host}:9153/health",
        f"http://{host}:9153/reset",
    ]
    assert manager.tool_endpoints[0]["endpoint_url"] == f"http://{host}:9153/search"
    popen.assert_not_called()


def test_mcp_wrapper_retains_direct_script_help() -> None:
    script = Path(parallel.__file__).with_name("mcp_mock_services.py")
    result = subprocess.run(
        [sys.executable, str(script), "--help"],
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "--task-yaml" in result.stdout
