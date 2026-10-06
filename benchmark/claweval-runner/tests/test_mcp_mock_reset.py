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

"""Tests for the audit HTTP server's per-service reset endpoint.

Batch workers run mock services with a port offset, so every URL taken from
task.yaml must be shifted before it is called — including the per-service
``/<service>/reset`` route of the audit server.
"""

from __future__ import annotations

import sys
import urllib.request
from pathlib import Path
from unittest.mock import patch

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _manager(tmp_path, port_offset):
    from ce_runner.mcp_mock_services import MockServiceManager

    task_yaml = tmp_path / "task.yaml"
    task_yaml.write_text(
        "task_id: T001zh_test\n"
        "services:\n"
        "  - name: rss\n"
        "    port: 9109\n"
        "    command: python3 -m rss_mock\n"
        "    reset_endpoint: http://localhost:9109/rss/reset\n"
    )
    return MockServiceManager(task_yaml, port_offset=port_offset)


class TestPerServiceReset:
    def test_service_reset_applies_the_port_offset(self, tmp_path):
        """POST /<service>/reset must hit the offset port, like /reset does."""
        from ce_runner.mcp_mock_services import AuditHTTPServer

        manager = _manager(tmp_path, port_offset=50)
        server = AuditHTTPServer(manager, port=0)
        server.start()
        try:
            port = server.server.server_address[1]
            with patch("ce_runner.mcp_mock_services.httpx.post") as mock_post:
                mock_post.return_value.status_code = 200
                with urllib.request.urlopen(
                        f"http://127.0.0.1:{port}/rss/reset", data=b"{}") as resp:
                    assert resp.status == 200
            called_urls = [c.args[0] for c in mock_post.call_args_list]
            assert called_urls == ["http://localhost:9159/rss/reset"], (
                "the per-service reset must shift the task.yaml port by the "
                f"offset: called {called_urls}"
            )
        finally:
            server.stop()

    def test_global_reset_applies_the_port_offset(self, tmp_path):
        """Guard: POST /reset already shifts (reset_all behaviour)."""
        from ce_runner.mcp_mock_services import AuditHTTPServer

        manager = _manager(tmp_path, port_offset=50)
        server = AuditHTTPServer(manager, port=0)
        server.start()
        try:
            port = server.server.server_address[1]
            with patch("ce_runner.mcp_mock_services.httpx.post") as mock_post:
                mock_post.return_value.status_code = 200
                with urllib.request.urlopen(
                        f"http://127.0.0.1:{port}/reset", data=b"{}") as resp:
                    assert resp.status == 200
            called_urls = [c.args[0] for c in mock_post.call_args_list]
            assert called_urls == ["http://localhost:9159/rss/reset"]
        finally:
            server.stop()
