#!/usr/bin/env python3
"""Tests for the credential file modes written by install-claude-code.sh (#4911).

``--config`` stores the DashScope API key as ``ANTHROPIC_AUTH_TOKEN`` in
cleartext inside ``~/.claude/settings.json``, and copies any previous settings
file to a timestamped ``settings.json.bak.<epoch>`` beside it. Both must come
out owner-only (0600): created by a plain ``cat >``/``cp`` they inherit the
caller's umask, so the default 022 leaves the current *and* the rotated key
readable by every local user.

The installer is driven end to end with rpm, sudo, dnf, curl and claude stubbed
on PATH - no network, no package manager, no root and no real Claude Code
install. Baseline on unchanged main: both mode assertions fail with 0644.
"""

import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

INSTALLER = Path(__file__).resolve().parent / "install-claude-code.sh"

API_KEY = "sk-test-placeholder-not-a-real-key"
ROTATED_KEY = "sk-rotated-placeholder-not-a-real-key"

# The native install path runs `curl -fsSL https://claude.ai/install.sh | bash`
# and then looks for a runnable `claude`: feed it a no-op "installer" and drop a
# stub binary so that path succeeds offline.
CURL_STUB = """#!/usr/bin/env bash
mkdir -p "$HOME/.local/bin"
printf '#!/usr/bin/env bash\\necho "9.9.9-stub"\\n' > "$HOME/.local/bin/claude"
chmod 755 "$HOME/.local/bin/claude"
echo ':'
"""

STUBS = {
    "rpm": "#!/usr/bin/env bash\nexit 0\n",   # every prerequisite looks installed
    "sudo": "#!/usr/bin/env bash\nexit 0\n",  # never touch the package manager
    "dnf": "#!/usr/bin/env bash\nexit 0\n",
    "curl": CURL_STUB,                        # never touch the network
    "claude": '#!/usr/bin/env bash\necho "9.9.9-stub"\n',
}


def mode_of(path):
    return stat.S_IMODE(path.stat().st_mode)


@unittest.skipUnless(os.name == "posix", "POSIX file modes are required")
@unittest.skipUnless(shutil.which("bash"), "bash is required to run the installer")
class ClaudeCodeConfigPermissionsTestCase(unittest.TestCase):
    def run_installer(self, home):
        stub_dir = home / "stubs"
        stub_dir.mkdir(parents=True, exist_ok=True)
        for name, source in STUBS.items():
            stub = stub_dir / name
            stub.write_text(source)
            stub.chmod(0o755)

        env = {
            "HOME": str(home),
            "PATH": os.pathsep.join([str(stub_dir), os.environ.get("PATH", os.defpath)]),
            "CLAUDE_API_KEY": API_KEY,
            "LC_ALL": "C",
        }
        # umask 022 is the default on the target OS; force it so the assertions
        # do not depend on the umask the suite happens to run under.
        return subprocess.run(
            [
                "bash", "-c", 'umask 022; exec bash "$@"', "install-claude-code.sh",
                str(INSTALLER), "--config", "--skip-tokenless",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=180,
        )

    def test_settings_file_is_owner_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            result = self.run_installer(home)
            output = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, output)

            settings = home / ".claude" / "settings.json"
            self.assertTrue(settings.is_file(), output)
            self.assertIn(API_KEY, settings.read_text())
            self.assertEqual(
                mode_of(settings),
                0o600,
                "settings.json holds ANTHROPIC_AUTH_TOKEN in cleartext and must be owner-only",
            )

    def test_backup_of_rotated_key_is_owner_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            claude_dir = home / ".claude"
            claude_dir.mkdir(parents=True)
            settings = claude_dir / "settings.json"
            settings.write_text(
                '{\n  "env": {\n    "ANTHROPIC_AUTH_TOKEN": "%s"\n  }\n}\n' % ROTATED_KEY
            )
            settings.chmod(0o644)

            result = self.run_installer(home)
            output = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, output)

            backups = sorted(claude_dir.glob("settings.json.bak.*"))
            self.assertEqual(len(backups), 1, "expected exactly one backup: %s" % backups)
            self.assertIn(ROTATED_KEY, backups[0].read_text())
            self.assertEqual(
                mode_of(backups[0]),
                0o600,
                "the backup retains the rotated token and must be owner-only",
            )
            self.assertEqual(
                mode_of(settings),
                0o600,
                "an existing 0644 settings.json must be tightened when it is rewritten",
            )

    def test_install_still_completes(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            result = self.run_installer(home)
            output = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, output)
            self.assertIn("Configuration written to", output)
            self.assertIn("Done!", output)


if __name__ == "__main__":
    unittest.main()
