"""Validate Hermes launch identity before registering native hooks or readiness."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch


class LaunchTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory(dir=os.environ["AW_TEST_OUTPUT"])
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.path = self.root / "launch.json"
        self.ready = self.root / "ready.json"
        self.data = {
            "schema": "aw-hermes/v1alpha1", "binary": "/bin/true",
            "binding": str(self.root / "binding"), "profile": str(self.root),
            "cwd": str(Path.cwd()), "ready_path": str(self.ready),
            "hooks": [{"event": "tool.before", "step": "test", "on_error": "block", "timeout": 1}],
            "token": "test-token", "revision": "a" * 40,
        }
        source = Path(__file__).resolve().parents[5] / "crates/aw-adapter/native/hermes/__init__.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_launch", source)
        self.plugin = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.plugin)
        self.version = SimpleNamespace(commit=None)
        shell = ModuleType("agent.shell_hooks")
        shell.ShellHookSpec = lambda **kwargs: SimpleNamespace(**kwargs)
        shell._make_callback = lambda spec: spec
        version = ModuleType("hermes_cli.version_info")
        version.get_version_info = lambda: self.version
        constants = ModuleType("hermes_constants")
        constants.get_hermes_home = lambda: self.root
        modules = patch.dict(sys.modules, {
            "agent.shell_hooks": shell, "hermes_cli.version_info": version,
            "hermes_constants": constants,
            "_aw_hermes_session": SimpleNamespace(launch_path=str(self.path)),
        })
        modules.start()
        self.addCleanup(modules.stop)
        self.hooks = []
        self.context = SimpleNamespace(register_hook=lambda *args: self.hooks.append(args))

    def register(self) -> None:
        self.path.write_text(json.dumps(self.data))
        self.path.chmod(0o600)
        self.plugin.register(self.context)

    def test_missing_or_malformed_revision_never_publishes_readiness(self) -> None:
        del self.data["revision"]
        with self.assertRaisesRegex(ValueError, "full Git object ID"):
            self.register()
        for revision in (None, "", 123, [], "a" * 39, "a" * 41, "a" * 63,
                         "a" * 65, "g" * 40, "a" * 39 + "\n", "０" * 40):
            with self.subTest(revision=revision):
                self.data["revision"] = revision
                with self.assertRaisesRegex(ValueError, "full Git object ID"):
                    self.register()
        self.assertEqual(self.hooks, [])
        self.assertFalse(self.ready.exists())

    def test_changed_or_unknown_native_revision_never_publishes_readiness(self) -> None:
        for commit in (None, "b" * 40):
            with self.subTest(commit=commit):
                self.version.commit = commit
                with self.assertRaisesRegex(ValueError, "checkout changed"):
                    self.register()
        self.assertEqual(self.hooks, [])
        self.assertFalse(self.ready.exists())

    def test_matching_full_git_ids_allow_other_revisions(self) -> None:
        for revision in ("a" * 40, "b" * 64, "ABCDEF0123" * 4):
            with self.subTest(revision=revision):
                self.data["revision"] = revision
                self.version.commit = revision
                self.register()
                self.assertEqual(json.loads(self.ready.read_text())["token"], "test-token")
                self.assertEqual(len(self.hooks), 1)
                self.hooks.clear()
                self.ready.unlink()


if __name__ == "__main__":
    unittest.main()
