"""Owned Hermes profile, dispatcher and callback fixtures."""

from __future__ import annotations
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import ModuleType, SimpleNamespace
from typing import Any, Callable
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from hermes_cli.plugins import PluginManager


class NativeFixture(unittest.TestCase):
    plugin_path: Path

    def setUp(self) -> None:
        self.addCleanup(sys.modules.pop, "_aw_hermes_session", None)
        self.addCleanup(setattr, sys, "pycache_prefix", sys.pycache_prefix)
        sys.pycache_prefix = None
        self.directory = tempfile.TemporaryDirectory(prefix="aw-hermes-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.home = self.root / "profile"
        self.home.mkdir(mode=0o700)
        self.trace = self.root / "trace.jsonl"
        self.command = self.root / "callback"
        shutil.copyfile(Path(__file__).with_name("native_command.py"), self.command)
        self.command.chmod(0o700)
        self.environment = patch.dict(
            os.environ,
            {
                "HERMES_HOME": str(self.home),
                "AW_HERMES_TRACE": str(self.trace),
                "HERMES_ENABLE_PROJECT_PLUGINS": "0",
                "HERMES_SAFE_MODE": "0",
            },
        )
        self.environment.start()
        self.addCleanup(self.environment.stop)
        from hermes_constants import set_hermes_home_override, reset_hermes_home_override

        token = set_hermes_home_override(self.home)
        self.addCleanup(reset_hermes_home_override, token)
        spec = importlib.util.spec_from_file_location(
            "aw_hermes_plugin", self.plugin_path / "__init__.py"
        )
        self.plugin = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.plugin)

    def launch(self, steps: list[tuple[str, str]]) -> Path:
        launch = self.root / "launch.json"
        launch.write_text(
            json.dumps(
                {
                    "schema": "aw-hermes/v1alpha1",
                    "profile": str(self.home),
                    "cwd": str(Path.cwd()),
                    "binary": str(self.command),
                    "binding": str(self.root / "binding.json"),
                    "ready_path": str(self.root / "ready.json"),
                    "token": "fixture-token",
                    "hooks": [
                        {"event": event, "step": step, "on_error": "report", "timeout": 3}
                        for event, step in steps
                    ],
                }
            )
        )
        launch.chmod(0o600)
        os.environ["AW_HERMES_LAUNCH"] = str(launch)
        session = ModuleType("_aw_hermes_session")
        session.launch_path = str(launch)
        sys.modules[session.__name__] = session
        return launch

    def manager(self) -> PluginManager:
        from hermes_cli.plugins import PluginManager

        return PluginManager(scope_key=str(self.home))

    def context(self, manager: PluginManager) -> SimpleNamespace:
        def register_hook(name: str, callback: Callable[..., Any]) -> None:
            manager._hooks.setdefault(name, []).append(callback)

        return SimpleNamespace(register_hook=register_hook)
