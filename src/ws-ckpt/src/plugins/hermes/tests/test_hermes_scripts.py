"""Run the Hermes lifecycle scripts with an isolated external CLI."""

import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[4] / "scripts" / "hermes"


class TestHermesScripts(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ws-ckpt-hermes-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.home = self.root / "hermes home"
        self.plugin = self.root / "payload/share/anolisa/runtime/ws-ckpt/plugins/hermes"
        self.plugin.mkdir(parents=True)
        for name in ("plugin.yaml", "__init__.py"):
            (self.plugin / name).touch()
        self.link = self.home / "plugins/ws-ckpt"
        self.link.parent.mkdir(parents=True)
        self.link.symlink_to(self.plugin, target_is_directory=True)
        self.skill = self.home / "skills/ws-ckpt"
        self.skill.mkdir(parents=True)
        (self.skill / "SKILL.md").touch()
        self.config = self.home / "config.yaml"
        self.config.write_text(
            "model: keep-model\nplugins:\n  enabled:\n    - ws-ckpt\n"
            "    - keep-plugin\n  ws-ckpt:\n    workspace: /work\n"
            "  keep-plugin:\n    option: keep-value\n"
        )
        self.calls = self.root / "calls.json"
        hermes = self.root / "hermes cli"
        hermes.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, signal, sys\n"
            "from pathlib import Path\n"
            "mode = os.environ['TEST_HERMES_MODE']\n"
            "if mode == 'hang':\n"
            "    signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
            "Path(os.environ['TEST_HERMES_CALLS']).write_text(json.dumps(\n"
            "    [sys.argv[1:], os.environ['HERMES_HOME'], os.getpid()]))\n"
            "if mode == 'hang':\n"
            "    while True: signal.pause()\n"
            "sys.exit(7 if mode == 'fail' else 0)\n"
        )
        hermes.chmod(0o755)
        self.env = {
            **os.environ,
            "HOME": str(self.root),
            "HERMES_HOME": str(self.home) + "/",
            "HERMES_BIN": str(hermes),
            "HERMES_SKILLS_DIR": str(self.home / "skills"),
            "HERMES_CONFIG_PATH": str(self.config),
            "ANOLISA_TARGET_DIR": str(self.root / "payload"),
            "ANOLISA_PROJECT_ROOT": "",
            "ANOLISA_DRY_RUN": "0",
            "TEST_HERMES_CALLS": str(self.calls),
        }

    def run_script(self, action, mode="success", dry_run=False):
        process = subprocess.Popen(
            ["bash", str(SCRIPTS / f"{action}-hermes.sh")],
            env={**self.env, "TEST_HERMES_MODE": mode,
                 "ANOLISA_DRY_RUN": "1" if dry_run else "0"},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            stdout, stderr = process.communicate(timeout=45)
        except subprocess.TimeoutExpired:
            # GNU timeout creates its own process group. Kill the stub too
            # so a missing --kill-after cannot leave the capture pipes open.
            if self.calls.exists():
                try:
                    os.kill(json.loads(self.calls.read_text())[2], signal.SIGKILL)
                except ProcessLookupError:
                    pass
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stdout, stderr = process.communicate(timeout=5)
            self.fail(f"{action} blocked for 45s\n{stdout}\n{stderr}")
        self.assertEqual(process.returncode, 0, stderr)
        if not dry_run:
            verb = "enable" if action == "install" else "disable"
            self.assertEqual(
                json.loads(self.calls.read_text())[:2],
                [["plugins", verb, "ws-ckpt"], str(self.home)],
            )
        return stdout, stderr

    def assert_removed(self):
        self.assertFalse(self.link.exists())
        self.assertFalse(self.link.is_symlink())
        self.assertFalse(self.skill.exists())
        self.assertTrue((self.plugin / "plugin.yaml").is_file())
        self.assertEqual(
            self.config.read_text(),
            "model: keep-model\nplugins:\n  enabled:\n    - keep-plugin\n"
            "  keep-plugin:\n    option: keep-value\n",
        )

    def test_uninstall_kills_unresponsive_cli_and_cleans_up(self):
        _, stderr = self.run_script("uninstall", "hang")
        self.assertIn("Warning:", stderr)
        self.assert_removed()

    def test_install_kills_unresponsive_cli_without_claiming_enabled(self):
        stdout, stderr = self.run_script("install", "hang")
        self.assertEqual(self.link.resolve(), self.plugin)
        self.assertIn("Warning:", stdout + stderr)
        self.assertNotIn("linked and enabled", stdout)

    def test_successful_cli(self):
        self.run_script("install")
        self.assertEqual(self.link.resolve(), self.plugin)
        self.run_script("uninstall")
        self.assert_removed()

    def test_failed_cli_remains_best_effort(self):
        stdout, stderr = self.run_script("install", "fail")
        self.assertIn("Warning:", stdout + stderr)
        self.assertNotIn("linked and enabled", stdout)
        self.assertEqual(self.link.resolve(), self.plugin)
        self.run_script("uninstall", "fail")
        self.assert_removed()

    def test_dry_run_does_not_call_cli_or_change_files(self):
        original_config = self.config.read_bytes()
        for action in ("install", "uninstall"):
            with self.subTest(action=action):
                stdout, _ = self.run_script(action, "hang", dry_run=True)
                self.assertIn("DRY-RUN:", stdout)
                self.assertFalse(self.calls.exists())
                self.assertEqual(self.config.read_bytes(), original_config)
                self.assertEqual(self.link.resolve(), self.plugin)
                self.assertTrue((self.skill / "SKILL.md").is_file())


if __name__ == "__main__":
    unittest.main()
