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

"""Both mock service launch paths preserve shell-style argv quoting."""

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

import pytest
import yaml
from ce_runner import parallel
from ce_runner.mcp_mock_services import MockServiceManager


@pytest.mark.parametrize("launcher", ["parallel", "mcp"])
@pytest.mark.parametrize(
    ("command", "expected"),
    [
        (
            'python "service script.py" --name "hello world"',
            ["python", "service script.py", "--name", "hello world"],
        ),
        (
            '"/opt/python runtime/bin/python" service.py',
            ["/opt/python runtime/bin/python", "service.py"],
        ),
        ("python service.py --empty ''", ["python", "service.py", "--empty", ""]),
        (
            r"python service.py --name hello\ world",
            ["python", "service.py", "--name", "hello world"],
        ),
        (
            "python -m mock_services.contacts --port 9103",
            ["python", "-m", "mock_services.contacts", "--port", "9103"],
        ),
    ],
)
def test_both_launchers_preserve_argv(
    tmp_path: Path, launcher: str, command: str, expected: list[str]
) -> None:
    service = {
        "name": "contacts",
        "port": 9103,
        "command": command,
        "health_check": "http://localhost:9103/health",
    }
    path = tmp_path / "tasks" / "T001" / "task.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(yaml.safe_dump({"services": [service]}), encoding="utf-8")

    if launcher == "parallel":
        with (
            patch.object(
                parallel.httpx,
                "post",
                side_effect=[
                    SimpleNamespace(status_code=503),
                    SimpleNamespace(status_code=200),
                ],
            ),
            patch.object(parallel.subprocess, "Popen") as popen,
        ):
            parallel.start_mock_services_with_offset(str(path), str(path.parent), 50)
    else:
        manager = MockServiceManager(path, port_offset=50)
        with (
            patch.object(manager, "_is_healthy", return_value=False),
            patch.object(manager, "_wait_health", return_value=True),
            patch("ce_runner.mcp_mock_services.time.sleep"),
            patch("ce_runner.mcp_mock_services.subprocess.Popen") as popen,
        ):
            manager.start_all()
        assert manager.processes == [popen.return_value]

    assert popen.call_args.args[0] == expected
    assert popen.call_args.kwargs["env"]["PORT"] == "9153"
    assert "shell" not in popen.call_args.kwargs


@pytest.mark.parametrize("launcher", ["parallel", "mcp"])
def test_unclosed_quotes_fail_before_launch(tmp_path: Path, launcher: str) -> None:
    service = {
        "name": "contacts",
        "port": 9103,
        "command": 'python "service.py',
        "health_check": "http://localhost:9103/health",
    }
    path = tmp_path / "tasks" / "T001" / "task.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(yaml.safe_dump({"services": [service]}), encoding="utf-8")
    popen = MagicMock()

    if launcher == "parallel":
        with (
            patch.object(
                parallel.httpx, "post", return_value=SimpleNamespace(status_code=503)
            ),
            patch.object(parallel.subprocess, "Popen", popen),
            patch.object(parallel.time, "monotonic", side_effect=[0, 100]),
        ):
            with pytest.raises(ValueError, match="quotation"):
                parallel.start_mock_services_with_offset(str(path), str(path.parent), 0)
    else:
        manager = MockServiceManager(path)
        with (
            patch.object(manager, "_is_healthy", return_value=False),
            patch.object(manager, "_wait_health", return_value=True),
            patch("ce_runner.mcp_mock_services.time.sleep"),
            patch("ce_runner.mcp_mock_services.subprocess.Popen", popen),
        ):
            with pytest.raises(ValueError, match="quotation"):
                manager.start_all()

    popen.assert_not_called()
