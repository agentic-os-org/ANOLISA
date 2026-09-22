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

"""Unit tests for src/ce_runner/preflight.py.

Covers:
- check_openclaw_plugins: healthy, structured plugin errors, fatal markers,
  missing command, timeout.
- check_docker: healthy, non-zero exit, missing command, timeout.
- run_preflight_checks: aggregation of both checks.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from unittest.mock import patch

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from ce_runner import preflight  # noqa: E402


def _completed(stdout="", stderr="", returncode=0):
    return subprocess.CompletedProcess(
        args=[], returncode=returncode, stdout=stdout, stderr=stderr
    )


class TestCheckOpenclawPlugins:
    def test_healthy(self):
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(stdout="No plugin issues detected.")):
            assert preflight.check_openclaw_plugins() == []

    def test_structured_plugin_error(self):
        out = (
            "Plugin errors:\n"
            '- lmstudio [load]: No "exports" main defined in /x/package.json\n'
        )
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(stdout=out)):
            errs = preflight.check_openclaw_plugins()
        assert len(errs) == 1
        assert "lmstudio" in errs[0]
        assert "load" in errs[0]

    def test_multiple_plugin_errors(self):
        out = (
            "Plugin errors:\n"
            "- foo [load]: boom\n"
            "- bar [init]: kaboom\n"
        )
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(stdout=out)):
            errs = preflight.check_openclaw_plugins()
        assert len(errs) == 2

    def test_fatal_marker_without_structured_line(self):
        out = "Failed to start CLI: PluginLoadFailureError: something"
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(stdout=out)):
            errs = preflight.check_openclaw_plugins()
        assert len(errs) == 1
        assert "Failed to start CLI" in errs[0]

    def test_exit_code_ignored_when_healthy(self):
        # Non-zero exit but clean output → no error (we don't trust exit code).
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(stdout="No plugin issues detected.",
                                                  returncode=1)):
            assert preflight.check_openclaw_plugins() == []

    def test_command_not_found(self):
        with patch.object(preflight.subprocess, "run",
                          side_effect=FileNotFoundError()):
            errs = preflight.check_openclaw_plugins()
        assert len(errs) == 1
        assert "not found" in errs[0]

    def test_timeout(self):
        with patch.object(preflight.subprocess, "run",
                          side_effect=subprocess.TimeoutExpired(cmd="openclaw", timeout=30)):
            errs = preflight.check_openclaw_plugins()
        assert len(errs) == 1
        assert "timed out" in errs[0]


class TestCheckDocker:
    def test_healthy(self):
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(returncode=0)):
            assert preflight.check_docker() == []

    def test_daemon_unreachable(self):
        err = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock."
        with patch.object(preflight.subprocess, "run",
                          return_value=_completed(stderr=err, returncode=1)):
            errs = preflight.check_docker()
        assert len(errs) == 1
        assert "docker daemon not reachable" in errs[0]

    def test_command_not_found(self):
        with patch.object(preflight.subprocess, "run",
                          side_effect=FileNotFoundError()):
            errs = preflight.check_docker()
        assert len(errs) == 1
        assert "not found" in errs[0]

    def test_timeout(self):
        with patch.object(preflight.subprocess, "run",
                          side_effect=subprocess.TimeoutExpired(cmd="docker", timeout=15)):
            errs = preflight.check_docker()
        assert len(errs) == 1
        assert "timed out" in errs[0]


class TestRunPreflightChecks:
    def test_all_healthy(self):
        with patch.object(preflight, "check_openclaw_plugins", return_value=[]), \
             patch.object(preflight, "check_docker", return_value=[]):
            ok, errs = preflight.run_preflight_checks()
        assert ok is True
        assert errs == []

    def test_aggregates_errors(self):
        with patch.object(preflight, "check_openclaw_plugins", return_value=["a"]), \
             patch.object(preflight, "check_docker", return_value=["b"]):
            ok, errs = preflight.run_preflight_checks()
        assert ok is False
        assert errs == ["a", "b"]


def _write_task(task_dir: Path, content: dict) -> str:
    """Write a task.yaml under *task_dir* and return its path."""
    task_dir.mkdir(parents=True, exist_ok=True)
    task_yaml = task_dir / "task.yaml"
    task_yaml.write_text(yaml.safe_dump(content))
    return str(task_yaml)


class TestFindMissingFixtures:
    def test_file_present_relative_to_task_dir(self, tmp_path):
        td = tmp_path / "tasks" / "M001"
        _write_task(td, {"task_id": "M001", "sandbox_files": ["fixtures/v.webm"]})
        (td / "fixtures").mkdir()
        (td / "fixtures" / "v.webm").write_bytes(b"\x00")
        assert preflight.find_missing_fixtures([str(td)]) == {}

    def test_file_missing_is_reported(self, tmp_path):
        td = tmp_path / "tasks" / "M002"
        tyaml = _write_task(td, {"task_id": "M002",
                                 "sandbox_files": ["fixtures/v.webm"]})
        result = preflight.find_missing_fixtures([str(td)])
        assert result == {tyaml: ["fixtures/v.webm"]}

    def test_project_root_fallback_resolves(self, tmp_path):
        # Cross-task reference: path relative to the project root (the nearest
        # ancestor containing a 'tasks/' dir), not the task's own dir.
        project = tmp_path
        td = project / "tasks" / "M003"
        _write_task(td, {"task_id": "M003",
                         "sandbox_files": ["tasks/shared/data.bin"]})
        shared = project / "tasks" / "shared"
        shared.mkdir(parents=True)
        (shared / "data.bin").write_bytes(b"\x00")
        assert preflight.find_missing_fixtures([str(td)]) == {}

    def test_legacy_environment_fixtures_fallback(self, tmp_path):
        td = tmp_path / "tasks" / "M004"
        tyaml = _write_task(td, {"task_id": "M004",
                                 "environment": {"fixtures": ["fixtures/x.mp4"]}})
        result = preflight.find_missing_fixtures([str(td)])
        assert result == {tyaml: ["fixtures/x.mp4"]}

    def test_no_files_declared_absent_from_result(self, tmp_path):
        td = tmp_path / "tasks" / "M005"
        _write_task(td, {"task_id": "M005", "sandbox_files": []})
        assert preflight.find_missing_fixtures([str(td)]) == {}

    def test_malformed_task_yaml_is_skipped(self, tmp_path):
        td = tmp_path / "tasks" / "M006"
        td.mkdir(parents=True)
        (td / "task.yaml").write_text("{ not: valid: yaml: ]]]")
        # Should not raise; malformed task simply contributes nothing.
        assert preflight.find_missing_fixtures([str(td)]) == {}
