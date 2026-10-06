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

"""Tests for MockServiceManager.call_tool error handling.

A tool declared in task.yaml ``tools`` without a matching entry in
``tool_endpoints`` has no URL; calling it must surface a JSON error to the
agent instead of raising.
"""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _manager(tmp_path, *, with_endpoint: bool):
    from ce_runner.mcp_mock_services import MockServiceManager

    task_yaml = tmp_path / "task.yaml"
    endpoints = ""
    if with_endpoint:
        endpoints = (
            "tool_endpoints:\n"
            "  - tool_name: search_feed\n"
            "    url: http://localhost:9109/rss/search\n"
            "    method: POST\n"
        )
    task_yaml.write_text(
        "task_id: T001zh_test\n"
        "services:\n"
        "  - name: rss\n"
        "    port: 9109\n"
        "    command: python3 -m rss_mock\n"
        "tools:\n"
        "  - name: search_feed\n"
        "    description: search\n"
        f"{endpoints}"
    )
    return MockServiceManager(task_yaml, port_offset=0)


class TestCallTool:
    def test_tool_without_endpoint_returns_json_error(self, tmp_path):
        """A tool missing its endpoint mapping must return an error, not raise.

        MockServiceManager registers every task tool; when task.yaml has no
        tool_endpoints entry for one, the registration succeeds but the entry
        carries no endpoint_url — call_tool then raised KeyError inside the
        MCP bridge.
        """
        from ce_runner.mcp_mock_services import MockServiceManager

        manager = _manager(tmp_path, with_endpoint=False)
        result = asyncio.run(manager.call_tool("search_feed", query="x"))
        parsed = json.loads(result)
        assert "error" in parsed, (
            f"expected a JSON error for the endpoint-less tool, got {parsed!r}"
        )
        assert "search_feed" in parsed["error"]

    def test_tool_with_endpoint_still_calls_through(self, tmp_path):
        """Guard: the normal path returns the mocked service response."""
        from unittest.mock import patch

        manager = _manager(tmp_path, with_endpoint=True)

        class _Resp:
            status_code = 200

            def json(self):
                return {"ok": True}

        async def fake_post(self, url, **kwargs):
            return _Resp()

        import httpx
        with patch.object(httpx.AsyncClient, "post", fake_post):
            result = asyncio.run(manager.call_tool("search_feed", query="x"))
        assert json.loads(result) == {"ok": True}
