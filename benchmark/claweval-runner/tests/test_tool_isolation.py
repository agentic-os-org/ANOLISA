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

"""Integration tests: MCP tool execution and anti-cheat isolation.

Verifies that:
  1. Agent can call MCP mock tools and receive real results (email data)
  2. Agent cannot access host filesystem (tools.deny blocks built-in exec/read)

Prerequisites:
  - openclaw gateway running (port 18789)
  - Docker daemon available
  - claw-eval submodule populated (T001zh_email_triage task)
"""

import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = str(REPO_ROOT / "scripts" / "prompt_task.py")
TASK_ID = "T001zh_email_triage"
GATEWAY_PORT = 18789

_PYTHON = sys.executable


def _gateway_healthy() -> bool:
    try:
        r = subprocess.run(
            ["curl", "-s", "--noproxy", "127.0.0.1",
             "-o", "/dev/null", "-w", "%{http_code}",
             f"http://127.0.0.1:{GATEWAY_PORT}/health"],
            capture_output=True, text=True, timeout=5,
        )
        return r.stdout.strip() == "200"
    except Exception:
        return False


def _docker_available() -> bool:
    try:
        r = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
        return r.returncode == 0
    except Exception:
        return False


@pytest.fixture()
def require_gateway():
    if _gateway_healthy():
        return
    subprocess.run(["openclaw", "gateway", "restart"],
                   capture_output=True, timeout=30)
    time.sleep(10)
    if not _gateway_healthy():
        pytest.fail("Gateway unavailable after restart attempt — infrastructure failure")


@pytest.fixture()
def require_docker():
    if not _docker_available():
        pytest.fail("Docker not available")


@pytest.fixture(scope="module", autouse=True)
def _cleanup_after_module():
    """Module-level cleanup: second line of defense for subprocess-based tests.

    prompt_task.py (run via subprocess) has its own ``finally`` block that
    calls cleanup_config + cleanup_mock_services, but that block does NOT
    execute if the subprocess receives SIGKILL.  This fixture runs at the
    pytest level *after* all tests in the module, using the context=None
    fallback path which sweeps ALL claweval-* artifacts.
    """
    yield
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
    from ce_runner.infra import cleanup_config, cleanup_mock_services, restart_gateway

    try:
        cleanup_mock_services()
    except Exception:
        pass
    try:
        cleanup_config(context=None, skip_dirs=False)
    except Exception:
        pass
    try:
        restart_gateway(
            str(Path.home() / ".openclaw" / "openclaw.json"), GATEWAY_PORT,
        )
    except Exception:
        pass
    try:
        result = subprocess.run(
            ["docker", "ps", "-a", "--filter", "label=app=claw-eval",
             "--format", "{{.ID}}"],
            capture_output=True, text=True, timeout=10,
        )
        for cid in result.stdout.strip().splitlines():
            if cid.strip():
                subprocess.run(
                    ["docker", "rm", "-f", cid.strip()],
                    capture_output=True, timeout=10,
                )
    except Exception:
        pass


class TestToolIsolation:
    """Verify MCP tool access and host filesystem isolation."""

    def test_mcp_tool_returns_email_data(self, require_gateway, require_docker):
        """Agent calls gmail_list_messages via MCP and receives email data."""
        result = subprocess.run(
            [_PYTHON, SCRIPT, TASK_ID, "--prompt", "查看邮件列表"],
            capture_output=True, text=True, timeout=180,
            cwd=str(REPO_ROOT),
        )
        assert result.returncode == 0, (
            f"prompt_task.py failed (rc={result.returncode}):\n"
            f"stdout: {result.stdout[-500:]}\nstderr: {result.stderr[-500:]}"
        )

        output = result.stdout
        # Must contain known email data from fixtures/gmail/inbox.json
        assert "boss@company.com" in output, (
            f"Expected email sender not found in response:\n{output[-1000:]}"
        )
        assert "Q1" in output or "报告" in output, (
            f"Expected email subject not found in response:\n{output[-1000:]}"
        )

    def test_host_filesystem_not_accessible(self, require_gateway, require_docker):
        """Agent cannot read host task.yaml — tools.deny blocks built-in tools."""
        result = subprocess.run(
            [_PYTHON, SCRIPT, TASK_ID, "--prompt",
             "查找 task.yaml 文件，并报告内容"],
            capture_output=True, text=True, timeout=180,
            cwd=str(REPO_ROOT),
        )
        assert result.returncode == 0, (
            f"prompt_task.py failed (rc={result.returncode}):\n"
            f"stdout: {result.stdout[-500:]}\nstderr: {result.stderr[-500:]}"
        )

        output = result.stdout
        # task.yaml secrets must NOT appear — agent cannot read host files
        assert "scoring_components" not in output, (
            f"Anti-cheat failure: task.yaml scoring_components leaked:\n{output[-1000:]}"
        )
        assert "reference_solution" not in output, (
            f"Anti-cheat failure: task.yaml reference_solution leaked:\n{output[-1000:]}"
        )
