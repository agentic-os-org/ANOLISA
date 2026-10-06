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

"""Tests for mock-service startup health reporting in batch mode.

`start_mock_services_with_offset` waits for each service's health endpoint
but used to return silently when the wait expired; the batch then failed
much later inside the agent run with confusing errors. The mcp_mock bridge
already logs the same condition (its _start_service twin).
"""

from __future__ import annotations

import sys
from pathlib import Path
from unittest.mock import patch

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _task(tmp_path, ready_timeout=0):
    task_yaml = tmp_path / "task.yaml"
    task_yaml.write_text(
        "task_id: T001zh_test\n"
        "services:\n"
        "  - name: gmail\n"
        "    port: 9100\n"
        "    command: python3 -m gmail_mock\n"
        "    health_check: http://localhost:9100/gmail/health\n"
        "    health_check_method: GET\n"
        f"    ready_timeout: {ready_timeout}\n"
    )
    return str(task_yaml)


class TestMockServiceHealthReporting:
    def test_unhealthy_service_logs_a_warning(self, tmp_path, monkeypatch):
        """A service that never turns healthy must be named in the log."""
        import httpx

        from ce_runner import parallel as parallel_mod

        # Popen must not actually spawn anything.
        monkeypatch.setattr(parallel_mod.subprocess, "Popen",
                            lambda *a, **k: None)

        def refuse(*args, **kwargs):
            raise httpx.ConnectError("refused")

        messages: list[str] = []
        with patch.object(parallel_mod.httpx, "get", side_effect=refuse), \
             patch.object(parallel_mod.httpx, "post", side_effect=refuse), \
             patch.object(parallel_mod, "log",
                          lambda m: messages.append(m)):
            parallel_mod.start_mock_services_with_offset(
                _task(tmp_path, ready_timeout=0), str(tmp_path), port_offset=50)

        assert any(
            "gmail" in m and "9150" in m for m in messages
        ), f"expected a warning naming the unhealthy service and port: {messages}"

    def test_healthy_service_is_silent(self, tmp_path, monkeypatch):
        """Guard: a service that answers 200 produces no warning."""
        from ce_runner import parallel as parallel_mod

        monkeypatch.setattr(parallel_mod.subprocess, "Popen",
                            lambda *a, **k: None)

        class _Resp:
            status_code = 200

        messages: list[str] = []
        with patch.object(parallel_mod.httpx, "get", return_value=_Resp()), \
             patch.object(parallel_mod.httpx, "post", return_value=_Resp()), \
             patch.object(parallel_mod, "log",
                          lambda m: messages.append(m)):
            parallel_mod.start_mock_services_with_offset(
                _task(tmp_path, ready_timeout=1), str(tmp_path), port_offset=50)

        assert not any("9150" in m and "not healthy" in m for m in messages)
