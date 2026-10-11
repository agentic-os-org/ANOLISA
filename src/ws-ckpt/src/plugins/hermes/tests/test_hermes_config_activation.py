"""Rejected configuration updates must not redirect hooks, commands or cron."""

import copy
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from types import ModuleType
from typing import Any

import hermes
import pytest
from hermes import checkpoint_manager, cron, tools
from hermes.checkpoint_manager import CheckpointManager
from hermes.config import HermesPluginConfig


@dataclass
class ConfigHarness:
    manager: CheckpointManager
    old_workspace: str
    new_workspace: str
    stored: dict[str, Any]
    lines: list[str]
    writes: list[list[str]]
    commands: list[list[str]]
    failure: str = ""

    def load(self) -> dict[str, Any]:
        if self.failure == "load":
            raise OSError("configuration cannot be read")
        return copy.deepcopy(self.stored)

    def save(self, config: dict[str, Any]) -> None:
        if self.failure == "save":
            raise PermissionError("configuration is read-only")
        self.stored.clear()
        self.stored.update(copy.deepcopy(config))

    def write_cron(self, lines: list[str]) -> bool:
        self.writes.append(list(lines))
        self.lines[:] = lines
        return True

    def run_command(self, argv: list[str]) -> tuple[bool, str]:
        self.commands.append(list(argv))
        return True, "checkpoint created"


@pytest.fixture
def config_harness(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> ConfigHarness:
    old_workspace = str(tmp_path / "old-workspace")
    new_workspace = str(tmp_path / "new-workspace")
    config = HermesPluginConfig(old_workspace, False, ["0 * * * *"])
    harness = ConfigHarness(
        manager=CheckpointManager(config),
        old_workspace=old_workspace,
        new_workspace=new_workspace,
        stored={
            "plugins": {
                "ws-ckpt": {
                    "workspace": old_workspace,
                    "autoCheckpoint": False,
                    "cronSchedules": ["0 * * * *"],
                }
            }
        },
        lines=[cron._build_cron_line(old_workspace, "0 * * * *")],
        writes=[],
        commands=[],
    )
    host_config = ModuleType("hermes_cli.config")
    host_config.is_managed = lambda: harness.failure == "managed"
    host_config.load_config = harness.load
    host_config.save_config = harness.save
    monkeypatch.setitem(sys.modules, "hermes_cli", ModuleType("hermes_cli"))
    monkeypatch.setitem(sys.modules, "hermes_cli.config", host_config)
    monkeypatch.setattr(checkpoint_manager, "_manager", harness.manager)
    monkeypatch.setattr(cron, "_LOCK_PATH", str(tmp_path / "cron.lock"))
    monkeypatch.setattr(cron, "_read_crontab", lambda: list(harness.lines))
    monkeypatch.setattr(cron, "_write_crontab", harness.write_cron)
    monkeypatch.setattr(tools, "_run_ws_ckpt_cmd", harness.run_command)
    return harness


@pytest.mark.parametrize("failure", ["managed", "load", "save"])
@pytest.mark.parametrize("key", ["workspace", "cronSchedules"])
def test_rejected_update_keeps_active_configuration(
    config_harness: ConfigHarness, failure: str, key: str
) -> None:
    harness = config_harness
    harness.failure = failure
    original_config = copy.deepcopy(harness.manager.config)
    original_stored = copy.deepcopy(harness.stored)
    original_lines = list(harness.lines)
    value = harness.new_workspace if key == "workspace" else 'set ["30 * * * *"]'

    result = json.loads(
        tools.handle_ws_ckpt_config({"action": "update", "key": key, "value": value})
    )

    assert result["success"] is False
    assert "Failed to persist config" in result["error"]
    assert harness.manager.config == original_config
    assert harness.stored == original_stored
    assert harness.lines == original_lines
    assert harness.writes == []

    # Follow the real hook and command resolution after the rejected update.
    hermes._on_session_start()
    assert cron.CrontabManager.list_installed(harness.old_workspace) == ["0 * * * *"]
    assert cron.CrontabManager.list_installed(harness.new_workspace) == []
    followup = json.loads(tools.handle_ws_ckpt_checkpoint({"id": "followup"}))
    assert followup["success"] is True
    assert harness.commands[0][2:4] == ["-w", harness.old_workspace]


@pytest.mark.parametrize("key", ["workspace", "cronSchedules"])
@pytest.mark.parametrize("cron_failure", [False, True])
def test_saved_update_activates_configuration_and_preserves_cron_warnings(
    config_harness: ConfigHarness, monkeypatch: pytest.MonkeyPatch, key: str, cron_failure: bool
) -> None:
    harness = config_harness
    if cron_failure:
        monkeypatch.setattr(cron, "_write_crontab", lambda _lines: False)
    value = harness.new_workspace if key == "workspace" else 'set ["30 * * * *"]'

    result = json.loads(
        tools.handle_ws_ckpt_config({"action": "update", "key": key, "value": value})
    )

    assert result["success"] is True
    assert ("WARNING" in result["output"]) is cron_failure
    expected_workspace = harness.new_workspace if key == "workspace" else harness.old_workspace
    expected_schedules = ["0 * * * *"] if key == "workspace" else ["30 * * * *"]
    assert harness.manager.config.workspace == expected_workspace
    assert harness.manager.config.cron_schedules == expected_schedules
    assert harness.stored["plugins"]["ws-ckpt"]["workspace"] == expected_workspace
    assert harness.stored["plugins"]["ws-ckpt"]["cronSchedules"] == expected_schedules
    followup = json.loads(tools.handle_ws_ckpt_checkpoint({"id": "followup"}))
    assert followup["success"] is True
    assert harness.commands[0][2:4] == ["-w", expected_workspace]
    if not cron_failure:
        assert cron.CrontabManager.list_installed(expected_workspace) == expected_schedules
        if key == "workspace":
            assert cron.CrontabManager.list_installed(harness.old_workspace) == []


def test_auto_checkpoint_persistence_failure_keeps_existing_setting(
    config_harness: ConfigHarness,
) -> None:
    harness = config_harness
    harness.manager.set_auto_checkpoint(True)
    harness.stored["plugins"]["ws-ckpt"]["autoCheckpoint"] = True
    harness.failure = "managed"

    result = json.loads(
        tools.handle_ws_ckpt_config({"action": "update", "key": "autoCheckpoint", "value": "false"})
    )

    assert result["success"] is False
    assert harness.manager.config.auto_checkpoint is True
    assert harness.stored["plugins"]["ws-ckpt"]["autoCheckpoint"] is True
    assert harness.writes == []
