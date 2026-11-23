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

"""Readiness handling of start_mock_services_with_offset."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from ce_runner import parallel


def _write_task(tmp_path, ready_timeout: int) -> str:
    task_yaml = tmp_path / "task.yaml"
    task_yaml.write_text(
        "services:\n"
        "  - name: judge-mock\n"
        "    port: 8010\n"
        "    command: python -m http.server 8010\n"
        "    health_check: http://localhost:8010/healthz\n"
        "    health_check_method: GET\n"
        f"    ready_timeout: {ready_timeout}\n"
    )
    return str(task_yaml)


def test_unready_service_raises_instead_of_continuing(tmp_path, monkeypatch) -> None:
    task_yaml = _write_task(tmp_path, 0)
    response = MagicMock(status_code=503)
    monkeypatch.setattr(parallel.httpx, "get", lambda *a, **k: response)
    monkeypatch.setattr(parallel.subprocess, "Popen", MagicMock())
    with pytest.raises(RuntimeError, match="judge-mock"):
        parallel.start_mock_services_with_offset(task_yaml, str(tmp_path), 0)


def test_ready_service_short_circuits_without_spawn(tmp_path, monkeypatch) -> None:
    task_yaml = _write_task(tmp_path, 15)
    response = MagicMock(status_code=200)
    monkeypatch.setattr(parallel.httpx, "get", lambda *a, **k: response)
    popen = MagicMock()
    monkeypatch.setattr(parallel.subprocess, "Popen", popen)
    parallel.start_mock_services_with_offset(task_yaml, str(tmp_path), 0)
    popen.assert_not_called()
