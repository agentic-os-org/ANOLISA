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

"""Test kill_mcp_bridges — leaked openclaw MCP stdio bridge reaper.

Covers ce_runner.infra.kill_mcp_bridges:
- Filters to processes whose argv contains ce_runner.mcp_mock_services or
  ce_runner.mcp_sandbox_tools
- Optional task_yaml argument restricts to bridges whose argv references that
  exact absolute path
- Returns the number of pids reaped
- Real-process smoke test: spawn a sleeper with the matching argv and confirm
  it is reaped within a couple of seconds
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path
from unittest.mock import MagicMock, patch

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _ps_output(lines: list[str]) -> MagicMock:
    """Build a fake subprocess.run result with a ps-style header + rows."""
    result = MagicMock()
    result.stdout = "  PID  PPID  PGID COMMAND\n" + "\n".join(lines) + "\n"
    return result


class TestKillMcpBridges:
    def test_matches_mock_services_bridge(self):
        from ce_runner.infra import kill_mcp_bridges

        lines = [
            "100 1 100 python -m ce_runner.mcp_mock_services --task-yaml /abs/x.yaml",
            "200 1 200 python -m ce_runner.mcp_sandbox_tools --task-yaml /abs/x.yaml",
            "300 1 300 openclaw gateway",
            "400 1 400 python unrelated_script.py",
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            n = kill_mcp_bridges()

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {100, 200}
        assert n == 2

    def test_task_yaml_filter(self):
        from ce_runner.infra import kill_mcp_bridges

        lines = [
            "100 1 100 python -m ce_runner.mcp_mock_services --task-yaml /abs/x.yaml",
            "200 1 200 python -m ce_runner.mcp_sandbox_tools --task-yaml /abs/y.yaml",
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            n = kill_mcp_bridges("/abs/x.yaml")

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {100}
        assert n == 1

    def test_skips_self_and_init(self):
        from ce_runner.infra import kill_mcp_bridges

        lines = [
            "1 0 1 python -m ce_runner.mcp_mock_services --task-yaml /abs/x.yaml",
            "99999 1 99999 python -m ce_runner.mcp_mock_services --task-yaml /abs/x.yaml",
            "200 1 200 python -m ce_runner.mcp_mock_services --task-yaml /abs/x.yaml",
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            n = kill_mcp_bridges()

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {200}
        assert n == 1

    def test_ps_failure_returns_zero(self):
        from ce_runner.infra import kill_mcp_bridges

        with patch("ce_runner.infra.subprocess.run",
                   side_effect=OSError("ps not found")), \
             patch("ce_runner.infra.os.killpg") as mock_killpg:
            n = kill_mcp_bridges()

        mock_killpg.assert_not_called()
        assert n == 0

    def test_real_process_reaped(self):
        """Smoke test: spawn a sleeper carrying the bridge marker in its argv
        and confirm kill_mcp_bridges('/tmp/fake.yaml') terminates it.
        """
        from ce_runner.infra import kill_mcp_bridges

        # argv: real python interpreter, a tiny -c that just sleeps, then
        # extra positional tokens that ps will surface so our scanner can
        # match on argv substrings.
        proc = subprocess.Popen(
            [
                sys.executable,
                "-c", "import time; time.sleep(60)",
                "ce_runner.mcp_mock_services",
                "--task-yaml", "/tmp/fake.yaml",
            ],
            start_new_session=True,
        )
        try:
            # Give the OS a moment to register the new process.
            time.sleep(0.2)

            n = kill_mcp_bridges("/tmp/fake.yaml")
            assert n == 1, f"expected 1 reaped, got {n}"

            # Wait for the process to exit (SIGTERM should be quick).
            for _ in range(20):
                if proc.poll() is not None:
                    break
                time.sleep(0.1)
            assert proc.poll() is not None, "sleeper did not exit after SIGTERM"
        finally:
            if proc.poll() is None:
                try:
                    proc.kill()
                except ProcessLookupError:
                    pass
                proc.wait(timeout=5)
