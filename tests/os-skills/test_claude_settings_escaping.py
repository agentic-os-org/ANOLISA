#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for install-claude-code.sh settings.json escaping.

write_config() interpolates the API key into a heredoc that is also
JSON, without escaping it for JSON first: a key containing a double
quote breaks the document (every later json parse of settings.json
fails), and a backslash forms an invalid escape. Dollar signs and
backticks in the value are inserted literally by the heredoc (parameter
expansion results are not re-scanned), which the second test pins down
as a control.

The installer runs end-to-end against a fixture HOME with stubbed
curl/rpm and a pre-created ~/.local/bin/claude, so the native-install
path succeeds without network. Baseline on unchanged main: 1 failure /
2 passing controls.
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

SPECIAL_KEY = 'sk-test"quote$dollar`tick\\slash'
PLAIN_KEY = "sk-plain-123456abcdef"


class ClaudeSettingsEscapingTests(unittest.TestCase):
    def setUp(self):
        if not Path("/bin/bash").exists():
            raise unittest.SkipTest("/bin/bash not present on this host")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / "home"
        (self.home / ".local" / "bin").mkdir(parents=True)
        # The native installer's success path only checks this file.
        claude = self.home / ".local" / "bin" / "claude"
        claude.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        claude.chmod(0o755)
        self.stubs = self.root / "stubs"
        self.stubs.mkdir()
        for name, text in (
            ("curl", "#!/bin/sh\nexit 0\n"),
            ("rpm", "#!/bin/sh\necho fixture-package\nexit 0\n"),
        ):
            path = self.stubs / name
            path.write_text(text, encoding="utf-8")
            path.chmod(0o755)

    def run_installer(self, api_key):
        env = dict(os.environ)
        env["PATH"] = f"{self.stubs}:/usr/bin:/bin"
        env["HOME"] = str(self.home)
        env["CLAUDE_API_KEY"] = api_key
        return subprocess.run(
            [
                "/bin/bash",
                str(SCRIPT),
                "--config",
                "--skip-tokenless",
            ],
            capture_output=True,
            text=True,
            timeout=120,
            env=env,
        )

    def read_settings(self):
        path = self.home / ".claude" / "settings.json"
        self.assertTrue(path.is_file(), f"settings.json missing: {path}")
        return json.loads(path.read_text(encoding="utf-8"))

    def test_special_characters_in_key_survive(self):
        result = self.run_installer(SPECIAL_KEY)
        self.assertEqual(result.returncode, 0, result.stdout[-600:] + result.stderr[-300:])
        settings = self.read_settings()
        self.assertEqual(settings["env"]["ANTHROPIC_AUTH_TOKEN"], SPECIAL_KEY)
        self.assertEqual(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "https://dashscope.aliyuncs.com/apps/anthropic",
        )

    def test_dollar_and_backtick_in_key_are_not_expanded(self):
        result = self.run_installer("sk-$HOME-`id -u`-key")
        self.assertEqual(result.returncode, 0, result.stdout[-600:] + result.stderr[-300:])
        settings = self.read_settings()
        self.assertEqual(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-$HOME-`id -u`-key")

    def test_plain_key_settings_are_unchanged(self):
        """Control: the ordinary alphanumeric key path keeps working."""
        result = self.run_installer(PLAIN_KEY)
        self.assertEqual(result.returncode, 0, result.stdout[-600:] + result.stderr[-300:])
        settings = self.read_settings()
        self.assertEqual(settings["env"]["ANTHROPIC_AUTH_TOKEN"], PLAIN_KEY)
        self.assertEqual(settings["env"]["ANTHROPIC_MODEL"], "qwen3-coder-plus")


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
