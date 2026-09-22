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

"""Test agent CLI process-group isolation and reclamation (memory-leak fix).

Covers Layer 1 of the orphan-process fix in ce_runner.agent:
- The openclaw CLI is launched with ``start_new_session=True`` so it becomes
  a process-group leader (lets us kill detached grandchildren as a group).
- The process group is always SIGKILL-reaped in ``finally`` so background
  grandchildren never survive a normal CLI exit.
- On timeout the group is SIGTERM'd, then escalated to SIGKILL if the CLI
  does not exit within the grace window.

All OS-level process calls are mocked so the test never spawns real work.
"""

import signal
import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _write_text_task(tmp_path: Path) -> str:
    """A minimal text-only task (no attachments => CLI path)."""
    task_yaml = tmp_path / "task.yaml"
    task_yaml.write_text(
        "task_id: T001_demo\n"
        "prompt:\n"
        "  text: hello world\n"
    )
    return str(task_yaml)


def _fake_popen(pid: int = 4242, *, timeout_on_communicate: bool = False,
                wait_times_out: bool = False) -> MagicMock:
    """Build a fake Popen object with controllable communicate/wait behaviour."""
    proc = MagicMock()
    proc.pid = pid
    if timeout_on_communicate:
        import subprocess
        proc.communicate.side_effect = subprocess.TimeoutExpired(cmd="openclaw", timeout=1)
    else:
        proc.communicate.return_value = (b"", b"")
    if wait_times_out:
        import subprocess
        # First wait() (grace window) times out, forcing SIGKILL escalation;
        # the final wait() after SIGKILL returns normally.
        proc.wait.side_effect = [
            subprocess.TimeoutExpired(cmd="openclaw", timeout=2), 0,
        ]
    else:
        proc.wait.return_value = 0
    return proc


class TestAgentProcessGroupIsolation:
    """Layer 1: the CLI runs in its own session and is group-reaped."""

    def test_cli_launched_in_new_session(self, tmp_path):
        """run_agent must spawn the CLI with start_new_session=True."""
        from ce_runner.agent import run_agent

        task_yaml = _write_text_task(tmp_path)
        proc = _fake_popen()

        with patch("ce_runner.agent.subprocess.Popen", return_value=proc) as mock_popen, \
             patch("ce_runner.agent.os.killpg"), \
             patch("ce_runner.agent.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.agent._extract_session_id_from_result", return_value=""), \
             patch("ce_runner.agent._find_session_file", return_value="/tmp/s.jsonl"):
            run_agent("claweval-T001-t1", task_yaml, timeout=10)

        assert mock_popen.call_args.kwargs.get("start_new_session") is True

    def test_process_group_reaped_on_normal_exit(self, tmp_path):
        """Even on a clean CLI exit, the process group is SIGKILL-reaped."""
        from ce_runner.agent import run_agent

        task_yaml = _write_text_task(tmp_path)
        proc = _fake_popen(pid=4242)

        with patch("ce_runner.agent.subprocess.Popen", return_value=proc), \
             patch("ce_runner.agent.os.killpg") as mock_killpg, \
             patch("ce_runner.agent.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.agent._extract_session_id_from_result", return_value=""), \
             patch("ce_runner.agent._find_session_file", return_value="/tmp/s.jsonl"):
            run_agent("claweval-T001-t1", task_yaml, timeout=10)

        # finally-block cleanup sends SIGKILL to the group.
        mock_killpg.assert_any_call(4242, signal.SIGKILL)

    def test_timeout_escalates_sigterm_then_sigkill(self, tmp_path):
        """On timeout: SIGTERM the group, then SIGKILL when grace wait expires."""
        from ce_runner.agent import run_agent

        task_yaml = _write_text_task(tmp_path)
        proc = _fake_popen(pid=777, timeout_on_communicate=True, wait_times_out=True)

        with patch("ce_runner.agent.subprocess.Popen", return_value=proc), \
             patch("ce_runner.agent.os.killpg") as mock_killpg, \
             patch("ce_runner.agent.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.agent._extract_session_id_from_result", return_value=""), \
             patch("ce_runner.agent._find_session_file", return_value="/tmp/s.jsonl"):
            run_agent("claweval-T001-t1", task_yaml, timeout=1)

        signals = [c.args for c in mock_killpg.call_args_list]
        assert (777, signal.SIGTERM) in signals
        assert (777, signal.SIGKILL) in signals

    def test_reap_swallows_missing_group(self, tmp_path):
        """A ProcessLookupError during group reap must not propagate."""
        from ce_runner.agent import run_agent

        task_yaml = _write_text_task(tmp_path)
        proc = _fake_popen()

        with patch("ce_runner.agent.subprocess.Popen", return_value=proc), \
             patch("ce_runner.agent.os.getpgid", side_effect=ProcessLookupError()), \
             patch("ce_runner.agent.os.killpg"), \
             patch("ce_runner.agent._extract_session_id_from_result", return_value=""), \
             patch("ce_runner.agent._find_session_file", return_value="/tmp/s.jsonl"):
            # Should not raise even though getpgid fails.
            result = run_agent("claweval-T001-t1", task_yaml, timeout=10)

        assert result == "/tmp/s.jsonl"

    def test_continue_session_runs_in_new_session(self, tmp_path):
        """_run_agent_continue applies the same process-group isolation."""
        from ce_runner.agent import _run_agent_continue

        proc = _fake_popen()

        with patch("ce_runner.agent.subprocess.Popen", return_value=proc) as mock_popen, \
             patch("ce_runner.agent.os.killpg") as mock_killpg, \
             patch("ce_runner.agent.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.agent._extract_session_id_from_result", return_value=""), \
             patch("ce_runner.agent._find_session_file", return_value="/tmp/s.jsonl"):
            _run_agent_continue("claweval-T001-t1", "follow up", timeout=10)

        assert mock_popen.call_args.kwargs.get("start_new_session") is True
        mock_killpg.assert_any_call(4242, signal.SIGKILL)
