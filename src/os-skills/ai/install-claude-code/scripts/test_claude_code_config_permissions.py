#!/usr/bin/env python3
"""Checks that the Claude Code installer keeps its API token owner-only.

`write_config()` writes ~/.claude/settings.json with ANTHROPIC_AUTH_TOKEN in
cleartext. It used to inherit the calling shell's umask (0644), so the token
was readable by every local user, and the timestamped `.bak` copies were
0644 as well.

The installer is driven end-to-end with `rpm`, `curl` and `claude` stubbed
on PATH so the test is hermetic (no network, no package manager, no root).
"""

import glob
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("install-claude-code.sh")

CURL_STUB = """#!/bin/sh
# Emulate `curl https://claude.ai/install.sh | bash` by placing a fake binary.
mkdir -p "$HOME/.local/bin"
printf '%s' '#!/bin/sh
echo "fake-claude 0.0.0"
' > "$HOME/.local/bin/claude"
chmod +x "$HOME/.local/bin/claude"
"""

RPM_STUB = """#!/bin/sh
exit 0
"""


@unittest.skipUnless(os.name == "posix", "POSIX file modes required")
class ClaudeCodeConfigModeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.stubs = self.root / "bin"
        self.stubs.mkdir()
        for name, body in (("curl", CURL_STUB), ("rpm", RPM_STUB)):
            path = self.stubs / name
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)

    def _install(self, api_key="sk-test-secret"):
        env = os.environ.copy()
        env["HOME"] = str(self.home)
        env["PATH"] = f"{self.stubs}:{env.get('PATH', '')}"
        env["CLAUDE_API_KEY"] = api_key
        return subprocess.run(
            ["bash", str(SCRIPT), "--config", "--skip-tokenless"],
            capture_output=True,
            text=True,
            env=env,
        )

    def _mode(self, path):
        return stat.S_IMODE(Path(path).stat().st_mode)

    def test_settings_file_is_owner_only(self):
        result = self._install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        settings = self.home / ".claude" / "settings.json"
        self.assertTrue(settings.is_file())
        self.assertEqual(self._mode(settings), 0o600)
        self.assertIn("sk-test-secret", settings.read_text(encoding="utf-8"))

    def test_backup_is_owner_only(self):
        claude_dir = self.home / ".claude"
        claude_dir.mkdir()
        old = claude_dir / "settings.json"
        old.write_text('{"env": {"ANTHROPIC_AUTH_TOKEN": "sk-old-key"}}', encoding="utf-8")
        # A settings.json written by an editor or by the previous installer.
        old.chmod(0o644)

        result = self._install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self._mode(old), 0o600)

        backups = glob.glob(str(claude_dir / "settings.json.bak.*"))
        self.assertEqual(len(backups), 1, backups)
        self.assertEqual(self._mode(backups[0]), 0o600)
        self.assertIn("sk-old-key", Path(backups[0]).read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
