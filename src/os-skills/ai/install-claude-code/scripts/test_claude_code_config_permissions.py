#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
r"""Tests for the mode of the credential file install-claude-code.sh writes (#4911).

``--config`` stores the DashScope API key as ``ANTHROPIC_AUTH_TOKEN`` in
cleartext inside ``~/.claude/settings.json``. On a host that has python3 the
merge helper creates a fresh file 0600 and keeps the mode of an existing one --
``test_claude_code_settings_merge.py`` pins that path. The python3-less fallback
writes its document with a plain ``cat >`` instead, which inherits the caller's
umask: 0644 under the default 022, so the key was readable by every local user
on exactly the hosts where nothing else protects it.

These cases pin the mode of a freshly written settings.json on both paths. What
they deliberately do *not* assert is that an existing 0644 file gets tightened,
or that the ``.bak`` copy comes out 0600: the merge helper documents keeping the
mode the user chose for the file, and ``shutil.copy2`` carries it to the backup.

The installer is driven end to end with rpm, sudo, dnf, curl and claude stubbed
on PATH -- no network, no package manager, no root and no real Claude Code
install. Baseline on a tree without the fallback fix: the python3-less case
fails with 0644 (420 != 384).

Usage: python3 src/os-skills/ai/install-claude-code/scripts/test_claude_code_config_permissions.py
"""

import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

INSTALLER = Path(__file__).resolve().parent / "install-claude-code.sh"

API_KEY = "sk-test-placeholder-not-a-real-key"

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
    return stat.S_IMODE(os.stat(path).st_mode)


@unittest.skipUnless(os.name == "posix", "POSIX file modes are required")
@unittest.skipUnless(shutil.which("bash"), "bash is required to run the installer")
class ClaudeCodeConfigPermissionsTestCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.settings = self.home / ".claude" / "settings.json"

        stub_dir = self.root / "stubs"
        stub_dir.mkdir()
        for name, source in STUBS.items():
            stub = stub_dir / name
            stub.write_text(source, encoding="utf-8")
            stub.chmod(0o755)
        self.stub_dir = stub_dir

    def without_python3(self):
        """Every /usr/bin tool except python* -- a host that has no python3."""
        curated = self.root / "no-python3"
        curated.mkdir(exist_ok=True)
        for entry in Path("/usr/bin").iterdir():
            if entry.name.startswith("python"):
                continue
            target = curated / entry.name
            if not target.exists():
                target.symlink_to(entry)
        return str(curated)

    def run_installer(self, search_path=None):
        env = {
            "HOME": str(self.home),
            "PATH": os.pathsep.join(
                [str(self.stub_dir), str(self.home / ".local" / "bin"),
                 search_path if search_path is not None else os.environ.get("PATH", os.defpath)]
            ),
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

    def assert_owner_only_fresh_settings(self, result):
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("Configuration written to", output)
        self.assertTrue(self.settings.is_file(), output)
        self.assertEqual(json.loads(self.settings.read_text(encoding="utf-8"))
                         ["env"]["ANTHROPIC_AUTH_TOKEN"], API_KEY)
        self.assertEqual(
            mode_of(self.settings),
            0o600,
            "settings.json holds ANTHROPIC_AUTH_TOKEN in cleartext and must be owner-only",
        )

    def test_fresh_settings_are_owner_only_without_python3(self):
        # The fallback has no JSON parser to set the mode, so the file it
        # creates is the one that ends up world-readable under umask 022.
        result = self.run_installer(search_path=self.without_python3())
        self.assert_owner_only_fresh_settings(result)

    def test_fresh_settings_are_owner_only_with_python3(self):
        # Already correct on the merge path; pinned here so a future rewrite of
        # either branch cannot quietly regress the mode of a fresh file.
        if shutil.which("python3") is None:
            self.skipTest("python3 is required to exercise the merge path")
        self.assert_owner_only_fresh_settings(self.run_installer())


if __name__ == "__main__":
    unittest.main(verbosity=2)
