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

"""Test orphan agent process reaping (memory-leak fix).

Covers ``reap_orphan_agent_processes`` in ce_runner.infra:
- Only matches lines whose argv contains both "openclaw" and a claweval- marker
- ``orphans_only=True`` additionally requires PPID == 1
- Skips the runner's own pid; best-effort against missing processes

The OS-level kill calls are mocked so the test never touches real processes.
"""

import os
import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


def _ps_output(lines: list[str]) -> MagicMock:
    """Build a fake subprocess.run result with a ps-style header + rows."""
    result = MagicMock()
    result.stdout = "  PID  PPID COMMAND\n" + "\n".join(lines) + "\n"
    return result


class TestReapOrphanAgentProcesses:
    """Test selective reaping of stray openclaw agent processes."""

    def test_only_matches_claweval_marker(self):
        """Only lines with both 'openclaw' and 'claweval-' are reaped."""
        from ce_runner.infra import reap_orphan_agent_processes

        lines = [
            "100 1 openclaw agent --session-id claweval-T001-t1",  # match
            "200 1 openclaw gateway",                              # no marker
            "300 1 python mock_services claweval-T001",            # no openclaw
            "400 1 openclaw agent --session-id claweval-T002-t2",  # match
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            reap_orphan_agent_processes()

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {100, 400}

    def test_orphans_only_requires_ppid_1(self):
        """With orphans_only=True only PPID==1 processes are reaped."""
        from ce_runner.infra import reap_orphan_agent_processes

        lines = [
            "100 1 openclaw agent --session-id claweval-T001-t1",   # orphan
            "400 555 openclaw agent --session-id claweval-T002-t2",  # has parent
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            reap_orphan_agent_processes(orphans_only=True)

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {100}

    def test_skips_own_pid(self):
        """The runner never kills its own process group."""
        from ce_runner.infra import reap_orphan_agent_processes

        lines = ["12345 1 openclaw agent --session-id claweval-T001-t1"]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=12345):
            reap_orphan_agent_processes()

        mock_killpg.assert_not_called()

    def test_best_effort_on_missing_process(self):
        """A ProcessLookupError on one pid does not abort the sweep."""
        from ce_runner.infra import reap_orphan_agent_processes

        lines = [
            "100 1 openclaw agent --session-id claweval-T001-t1",
            "400 1 openclaw agent --session-id claweval-T002-t2",
        ]

        def _killpg(pgid, sig):
            if pgid == 100:
                raise ProcessLookupError()

        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg", side_effect=_killpg) as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            # Should not raise
            reap_orphan_agent_processes()

        attempted = {c.args[0] for c in mock_killpg.call_args_list}
        assert attempted == {100, 400}

    def test_uses_sigterm_on_process_group(self):
        """Reaping sends SIGTERM to the process *group*, not SIGKILL."""
        import signal

        from ce_runner.infra import reap_orphan_agent_processes

        lines = ["100 1 openclaw agent --session-id claweval-T001-t1"]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            reap_orphan_agent_processes()

        mock_killpg.assert_called_once_with(100, signal.SIGTERM)

    def test_falls_back_to_os_kill_when_no_pgid(self):
        """When pgid cannot be resolved, fall back to os.kill on the pid."""
        import signal

        from ce_runner.infra import reap_orphan_agent_processes

        lines = ["100 1 openclaw agent --session-id claweval-T001-t1"]

        def _killpg(pgid, sig):
            raise OSError("no such process group")

        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg", side_effect=_killpg), \
             patch("ce_runner.infra.os.kill") as mock_kill, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            reap_orphan_agent_processes()

        mock_kill.assert_called_once_with(100, signal.SIGTERM)

    def test_ps_failure_does_not_raise(self):
        """If ps itself fails, the sweep logs and returns without raising."""
        from ce_runner.infra import reap_orphan_agent_processes

        with patch("ce_runner.infra.subprocess.run",
                   side_effect=OSError("ps not found")), \
             patch("ce_runner.infra.os.killpg") as mock_killpg:
            # Should not raise
            reap_orphan_agent_processes()

        mock_killpg.assert_not_called()

    def test_skips_malformed_lines(self):
        """Rows missing pid/ppid/args columns are skipped, not crashed on."""
        from ce_runner.infra import reap_orphan_agent_processes

        lines = [
            "garbage-without-columns",                              # too few cols
            "notanumber 1 openclaw agent claweval-T001",            # bad pid
            "100 notanumber openclaw agent claweval-T001",          # bad ppid
            "200 1 openclaw agent --session-id claweval-T002-t2",   # valid match
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            reap_orphan_agent_processes()

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {200}

    def test_custom_markers(self):
        """A caller-supplied marker list overrides the default claweval-."""
        from ce_runner.infra import reap_orphan_agent_processes

        lines = [
            "100 1 openclaw agent --session-id claweval-T001-t1",   # default marker
            "200 1 openclaw agent --session-id custom-marker-xyz",  # custom marker
        ]
        with patch("ce_runner.infra.subprocess.run",
                   return_value=_ps_output(lines)), \
             patch("ce_runner.infra.os.getpgid", side_effect=lambda p: p), \
             patch("ce_runner.infra.os.killpg") as mock_killpg, \
             patch("ce_runner.infra.os.getpid", return_value=99999):
            reap_orphan_agent_processes(markers=["custom-marker-"])

        killed = {c.args[0] for c in mock_killpg.call_args_list}
        assert killed == {200}
