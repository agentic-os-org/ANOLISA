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

"""Batch cleanup must not destroy the user's global openclaw tools config.

ce-runner no longer writes the global ``tools`` section (tool policy lives in
per-agent entries since the always-sandbox refactor), so the cleanup-time
``config["tools"] = {"profile": "coding"}`` was a purely destructive
overwrite of user configuration.
"""

import json
import sys
import types
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "src"))


@pytest.fixture()
def stubbed_claw_eval(monkeypatch):
    """Stub the external claw_eval package used inside setup_parallel_workers."""
    modules = {}
    for name in [
        "claw_eval",
        "claw_eval.config",
        "claw_eval.models",
        "claw_eval.models.task",
        "claw_eval.runner",
        "claw_eval.runner.sandbox_runner",
    ]:
        modules[name] = types.ModuleType(name)

    class SandboxConfig:
        def __init__(self, image=None):
            self.image = image

    class TaskDefinition:
        sandbox_files = []
        local_grader_files = []

        @classmethod
        def from_yaml(cls, path):
            return cls()

    class SandboxRunner:
        def __init__(self, cfg):
            pass

        def start_container(self, run_id=None):
            raise NotImplementedError

        def stop_container(self, handle):
            pass

        def inject_files(self, *a, **k):
            return 0

    modules["claw_eval.config"].SandboxConfig = SandboxConfig
    modules["claw_eval.models.task"].TaskDefinition = TaskDefinition
    modules["claw_eval.runner.sandbox_runner"].SandboxRunner = SandboxRunner
    for name, mod in modules.items():
        monkeypatch.setitem(sys.modules, name, mod)


def _write_config(path: Path, tools_present: bool, tools=None) -> None:
    config = {
        "gateway": {"port": 18789},
        "agents": {"list": []},
        "mcp": {"servers": {}},
    }
    if tools_present:
        config["tools"] = tools
    path.write_text(json.dumps(config), encoding="utf-8")


def _task_yaml(tmp_path: Path) -> str:
    task_dir = tmp_path / "T001"
    task_dir.mkdir(exist_ok=True)
    task_yaml = task_dir / "task.yaml"
    task_yaml.write_text("task_id: T001\ntools: []\n", encoding="utf-8")
    return str(task_yaml)


class TestParallelCleanupRestoresUserTools:
    def test_user_tools_restored_verbatim(self, tmp_path, stubbed_claw_eval):
        from ce_runner.tool_injector import ToolInjector

        user_tools = {
            "profile": "coding",
            "allow": ["my_custom_tool"],
            "deny": ["dangerous_tool"],
        }
        cfg = tmp_path / "openclaw.json"
        _write_config(cfg, tools_present=True, tools=user_tools)

        injector = ToolInjector(str(cfg))
        setup_info = injector.setup_parallel_workers([_task_yaml(tmp_path)], 1)
        assert setup_info.get("original_tools") == user_tools
        injector.cleanup_parallel(setup_info, skip_dirs=True)

        after = json.loads(cfg.read_text(encoding="utf-8"))
        assert after.get("tools") == user_tools, (
            f"user tools destroyed by cleanup: {after.get('tools')}"
        )

    def test_absent_tools_stays_absent(self, tmp_path, stubbed_claw_eval):
        from ce_runner.tool_injector import ToolInjector

        cfg = tmp_path / "openclaw.json"
        _write_config(cfg, tools_present=False)

        injector = ToolInjector(str(cfg))
        setup_info = injector.setup_parallel_workers([_task_yaml(tmp_path)], 1)
        injector.cleanup_parallel(setup_info, skip_dirs=True)

        after = json.loads(cfg.read_text(encoding="utf-8"))
        assert "tools" not in after, (
            f"cleanup invented a tools section: {after.get('tools')}"
        )

    def test_legacy_setup_info_keeps_previous_default(self, tmp_path, stubbed_claw_eval):
        from ce_runner.tool_injector import ToolInjector

        cfg = tmp_path / "openclaw.json"
        _write_config(cfg, tools_present=False)

        injector = ToolInjector(str(cfg))
        setup_info = injector.setup_parallel_workers([_task_yaml(tmp_path)], 1)
        # Simulate a legacy caller's setup dict without the snapshot.
        legacy = {k: v for k, v in setup_info.items() if k != "original_tools"}
        injector.cleanup_parallel(legacy, skip_dirs=True)

        after = json.loads(cfg.read_text(encoding="utf-8"))
        assert after.get("tools") == {"profile": "coding"}


class TestInfraCleanupPreservesUserTools:
    def test_cleanup_config_preserves_existing_tools(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        from ce_runner.infra import cleanup_config

        user_tools = {"profile": "coding", "allow": ["keep_me"]}
        cfg = tmp_path / "openclaw.json"
        _write_config(cfg, tools_present=True, tools=user_tools)

        cleanup_config(config_path=str(cfg), skip_dirs=True)

        after = json.loads(cfg.read_text(encoding="utf-8"))
        assert after.get("tools") == user_tools
