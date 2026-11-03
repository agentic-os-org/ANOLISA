"""The Qoder TUI lane must stay interactive and trust only its private workspace."""

import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "qoder_reload", AW / "crates/aw-cli/tests/fixtures/qoder/reload.py"
)
reload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reload)


class QoderReloadEnvironmentTests(unittest.TestCase):
    def setUp(self) -> None:
        (AW / "target").mkdir(exist_ok=True)

    def test_ci_runner_gets_interactive_child_without_changing_parent_environment(self) -> None:
        with tempfile.TemporaryDirectory(dir=AW / "target", prefix="qoder-reload-env-") as directory, patch.dict(
            os.environ, {"CI": "true", "GITHUB_ACTIONS": "true", "HOME": "/original"}
        ):
            root = Path(directory)
            child = reload.tui_environment(root)
            self.assertNotIn("CI", child)
            self.assertNotIn("GITHUB_ACTIONS", child)
            self.assertEqual(child["TERM"], "xterm-256color")
            self.assertEqual(child["HOME"], str(root / "home"))
            self.assertEqual(os.environ["CI"], "true")
            self.assertEqual(os.environ["GITHUB_ACTIONS"], "true")
            self.assertEqual(os.environ["HOME"], "/original")

    def test_trust_is_scoped_to_each_private_workspace(self) -> None:
        with tempfile.TemporaryDirectory(dir=AW / "target", prefix="qoder-reload-env-") as directory:
            for name in ("first", "second"):
                root = Path(directory) / name
                root.mkdir()
                child = reload.tui_environment(root)
                home = Path(child["HOME"])
                settings = home / ".qoder/settings.json"
                self.assertEqual(json.loads(settings.read_text()), {
                    "permissions": {"trustDirectories": [str(root)]}
                })
                self.assertEqual(home.stat().st_mode & 0o777, 0o700)
                self.assertEqual(settings.stat().st_mode & 0o777, 0o600)
                self.assertFalse((root / ".qoder").exists())


if __name__ == "__main__":
    unittest.main()
