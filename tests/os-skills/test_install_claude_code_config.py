#!/usr/bin/env python3
"""Regression tests for the install-claude-code.sh configuration step (#3547).

``write_config`` used to ``return 1`` when no API key was supplied and to die
on the bare ``read -rp`` when stdin was closed.  The installer runs under
``set -euo pipefail`` and ``main`` calls ``write_config`` as a plain statement,
so declining the prompt aborted the whole run: the tokenless plugin step and
the final banner were never reached and the script exited 1.

The harness sources the real installer with its trailing ``main "$@"`` removed
(under the same ``set -euo pipefail``), calls ``write_config`` and then echoes
a marker that stands in for the step following the call in ``main``.

Baseline on unchanged main: the decline and closed-stdin scenarios exit 1 and
never print the marker.
"""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "ai"
    / "install-claude-code"
    / "scripts"
    / "install-claude-code.sh"
)

MAIN_CALL = '\nmain "$@"\n'
MARKER = "STEP6_REACHED"
SKIP_WARNING = "skipping configuration"


class InstallClaudeCodeConfigTestCase(unittest.TestCase):
    """Drive the installer's write_config under the script's own shell opts."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)

        self.home = root / "home"
        self.home.mkdir()
        self.settings = self.home / ".claude" / "settings.json"

        body = SCRIPT.read_text(encoding="utf-8")
        self.assertIn(MAIN_CALL, body, "installer no longer ends with main \"$@\"")
        self.lib = root / "installer-lib.sh"
        self.lib.write_text(body.replace(MAIN_CALL, "\n"), encoding="utf-8")

        self.driver = root / "driver.sh"
        self.driver.write_text(
            "set -euo pipefail\n"
            f'source "{self.lib}"\n'
            "write_config\n"
            f'echo "{MARKER}"\n',
            encoding="utf-8",
        )

    def run_driver(self, stdin_data=None, closed_stdin=False, api_key=None):
        env = {k: v for k, v in os.environ.items() if k != "CLAUDE_API_KEY"}
        env["HOME"] = str(self.home)
        env["PATH"] = os.environ.get("PATH", "")
        if api_key is not None:
            env["CLAUDE_API_KEY"] = api_key
        if closed_stdin:
            return subprocess.run(
                ["bash", str(self.driver)],
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                env=env,
                timeout=60,
            )
        return subprocess.run(
            ["bash", str(self.driver)],
            input=stdin_data if stdin_data is not None else "",
            capture_output=True,
            text=True,
            env=env,
            timeout=60,
        )

    def assertContinued(self, proc):
        self.assertEqual(
            proc.returncode,
            0,
            f"write_config aborted the install\nstdout: {proc.stdout}\nstderr: {proc.stderr}",
        )
        self.assertIn(MARKER, proc.stdout, "steps after write_config were skipped")


class TestDeclinedApiKey(InstallClaudeCodeConfigTestCase):
    def test_empty_answer_warns_and_continues(self):
        proc = self.run_driver(stdin_data="\n")
        self.assertContinued(proc)
        self.assertIn(SKIP_WARNING, proc.stdout)
        self.assertFalse(self.settings.exists(), "settings.json must not be written")

    def test_closed_stdin_warns_and_continues(self):
        proc = self.run_driver(closed_stdin=True)
        self.assertContinued(proc)
        self.assertIn(SKIP_WARNING, proc.stdout)
        self.assertFalse(self.settings.exists(), "settings.json must not be written")


class TestProvidedApiKey(InstallClaudeCodeConfigTestCase):
    def read_settings(self):
        self.assertTrue(self.settings.exists(), "settings.json was not written")
        return json.loads(self.settings.read_text(encoding="utf-8"))

    def test_env_key_is_written(self):
        proc = self.run_driver(api_key="sk-test-env-key")
        self.assertContinued(proc)
        self.assertEqual(
            self.read_settings()["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test-env-key"
        )

    def test_piped_key_is_written(self):
        proc = self.run_driver(stdin_data="sk-test-piped-key\n")
        self.assertContinued(proc)
        self.assertEqual(
            self.read_settings()["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test-piped-key"
        )

    def test_existing_settings_are_backed_up(self):
        self.settings.parent.mkdir(parents=True)
        self.settings.write_text('{"env":{}}', encoding="utf-8")
        proc = self.run_driver(api_key="sk-test-env-key")
        self.assertContinued(proc)
        backups = list(self.settings.parent.glob("settings.json.bak.*"))
        self.assertEqual(len(backups), 1, "existing settings were not backed up")
        self.assertEqual(backups[0].read_text(encoding="utf-8"), '{"env":{}}')


if __name__ == "__main__":
    unittest.main()
