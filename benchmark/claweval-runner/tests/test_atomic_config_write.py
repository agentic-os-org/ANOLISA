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

"""Unit tests for atomic_write_config and the refactored cleanup paths.

Verifies:
- atomic_write_config: writes valid JSON, is atomic (no partial writes
  observed), cleans up temp files on error.
- cleanup_config (infra.py): no openclaw CLI subprocesses spawned.
- ToolInjector.cleanup: no openclaw CLI subprocesses spawned.
- cleanup_parallel: no openclaw CLI subprocesses spawned.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from ce_runner._common import atomic_write_config  # noqa: E402


class TestAtomicWriteConfig:
    def test_writes_valid_json(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        data = {"gateway": {"port": 18789}, "agents": {"list": []}}
        atomic_write_config(config_path, data)

        with open(config_path) as f:
            result = json.load(f)
        assert result == data

    def test_trailing_newline(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        atomic_write_config(config_path, {"x": 1})

        raw = Path(config_path).read_text()
        assert raw.endswith("\n")

    def test_overwrites_existing(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        Path(config_path).write_text('{"old": true}')

        atomic_write_config(config_path, {"new": True})
        with open(config_path) as f:
            assert json.load(f) == {"new": True}

    def test_no_partial_write_on_error(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        original = {"original": True}
        Path(config_path).write_text(json.dumps(original))

        # Simulate write failure by making os.replace raise
        with patch("os.replace", side_effect=OSError("disk full")):
            with pytest.raises(OSError, match="disk full"):
                atomic_write_config(config_path, {"should_not_appear": True})

        # Original file should be untouched
        with open(config_path) as f:
            assert json.load(f) == original

        # No temp files left behind
        tmps = list(tmp_path.glob(".openclaw-cfg-*"))
        assert tmps == []

    def test_no_temp_file_left_on_success(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        atomic_write_config(config_path, {"clean": True})
        tmps = list(tmp_path.glob(".openclaw-cfg-*"))
        assert tmps == []

    def test_size_drop_overwrites_backup_files(self, tmp_path):
        """When config shrinks >50%, .last-good and .bak are overwritten."""
        config_path = str(tmp_path / "openclaw.json")
        # Write a large initial config (simulates MCP-bloated state)
        large_config = {"mcp": {"servers": {f"ce-mock-{i}": {} for i in range(100)}}}
        Path(config_path).write_text(json.dumps(large_config, indent=2))

        # Create stale backup files with old content
        stale = '{"stale": true, "gateway": {"mode": "local"}}'
        Path(config_path + ".last-good").write_text(stale)
        Path(config_path + ".bak").write_text(stale)

        # Write a much smaller clean config
        small_config = {"mcp": {"servers": {}}, "tools": {"profile": "coding"}}
        atomic_write_config(config_path, small_config)

        # .last-good and .bak should now contain the clean config
        with open(config_path + ".last-good") as f:
            lg = json.load(f)
        assert lg == small_config

        with open(config_path + ".bak") as f:
            bak = json.load(f)
        assert bak == small_config

    def test_backup_synced_when_size_stable(self, tmp_path):
        """Even when config size is stable, backups are synced to new content."""
        config_path = str(tmp_path / "openclaw.json")
        initial = {"mcp": {"servers": {"a": {}, "b": {}}}}
        Path(config_path).write_text(json.dumps(initial, indent=2))

        Path(config_path + ".last-good").write_text('{"stale": true}')
        Path(config_path + ".bak").write_text('{"stale": true}')

        new_config = {"mcp": {"servers": {"c": {}, "d": {}}}}
        atomic_write_config(config_path, new_config)

        with open(config_path + ".last-good") as f:
            assert json.load(f) == new_config
        with open(config_path + ".bak") as f:
            assert json.load(f) == new_config

    def test_backup_sync_on_grow(self, tmp_path):
        """When config grows, backups are still synced to the new content."""
        config_path = str(tmp_path / "openclaw.json")
        small_config = {"clean": True}
        Path(config_path).write_text(json.dumps(small_config))

        Path(config_path + ".last-good").write_text('{"old": true}')
        Path(config_path + ".bak").write_text('{"old": true}')

        large_config = {"mcp": {"servers": {f"srv-{i}": {} for i in range(50)}}}
        atomic_write_config(config_path, large_config)

        with open(config_path + ".last-good") as f:
            assert json.load(f) == large_config
        with open(config_path + ".bak") as f:
            assert json.load(f) == large_config

    def test_size_drop_no_backup_files_exist(self, tmp_path):
        """Size-drop path handles missing backup files gracefully."""
        config_path = str(tmp_path / "openclaw.json")
        large_config = {"data": "x" * 2000}
        Path(config_path).write_text(json.dumps(large_config))

        # No .last-good or .bak exist
        small_config = {"clean": True}
        atomic_write_config(config_path, small_config)

        # Should complete without error; main config is correct
        with open(config_path) as f:
            assert json.load(f) == small_config


class TestCleanupConfigNoCliCalls:
    """Verify cleanup_config does not spawn openclaw CLI subprocesses."""

    def test_no_openclaw_subprocess(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        config = {
            "agents": {"list": [
                {"id": "claweval-T001"},
                {"id": "claweval-T002"},
                {"id": "keep-me"},
            ]},
            "mcp": {"servers": {
                "claw-eval-T001": {"command": "node"},
                "claw-eval-T002": {"command": "node"},
                "user-server": {"command": "python"},
            }},
            "tools": {"profile": "minimal"},
        }
        Path(config_path).write_text(json.dumps(config))

        from ce_runner.infra import cleanup_config
        with patch("subprocess.run") as mock_run:
            cleanup_config(config_path=config_path, skip_dirs=True)

        # No openclaw agents delete or mcp unset calls
        mock_run.assert_not_called()

        # Verify config state
        with open(config_path) as f:
            result = json.load(f)
        agent_ids = [a["id"] for a in result["agents"]["list"]]
        assert "claweval-T001" not in agent_ids
        assert "claweval-T002" not in agent_ids
        assert "keep-me" in agent_ids
        assert "claw-eval-T001" not in result["mcp"]["servers"]
        assert "user-server" in result["mcp"]["servers"]
        assert result["tools"] == {"profile": "coding"}


class TestToolInjectorCleanupNoCliCalls:
    """Verify ToolInjector.cleanup does not spawn openclaw CLI subprocesses."""

    def test_no_openclaw_subprocess(self, tmp_path):
        config_path = str(tmp_path / "openclaw.json")
        config = {
            "agents": {"list": [
                {"id": "claweval-T001"},
                {"id": "other-agent"},
            ]},
            "mcp": {"servers": {
                "ce-mock-T001": {"command": "node"},
                "ce-sb-T001": {"command": "node"},
                "user-server": {"command": "python"},
            }},
            "tools": {"profile": "minimal"},
        }
        Path(config_path).write_text(json.dumps(config))

        from ce_runner.tool_injector import ToolInjector, ToolInjectionContext
        injector = ToolInjector(config_path)
        context = ToolInjectionContext(
            task_id="T001",
            agent_id="claweval-T001",
            mcp_name="ce-mock-T001",
            sandbox_mcp_name="ce-sb-T001",
        )

        with patch("subprocess.run") as mock_run, \
             patch("ce_runner.tool_injector.cleanup_task_skill"):
            injector.cleanup(context, skip_dirs=True)

        mock_run.assert_not_called()

        with open(config_path) as f:
            result = json.load(f)
        agent_ids = [a["id"] for a in result["agents"]["list"]]
        assert "claweval-T001" not in agent_ids
        assert "other-agent" in agent_ids
        assert "ce-mock-T001" not in result["mcp"]["servers"]
        assert "ce-sb-T001" not in result["mcp"]["servers"]
        assert "user-server" in result["mcp"]["servers"]
