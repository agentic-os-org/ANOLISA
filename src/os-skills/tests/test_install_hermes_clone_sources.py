#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for install-hermes clone sources.

The installer cloned only from the GitCode mirror, which answers 401 to an
anonymous clone on a host with no GitCode credentials. A clean host therefore
failed at the clone step with no way forward, even though the GitHub project
SKILL.md names is public.

`clone_repo` is driven here with a stub `git` on PATH, so the cases are about
which source is tried in which order — not about git or the network.
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

SCRIPTS = Path(__file__).resolve().parents[1] / "ai" / "install-hermes" / "scripts"
INSTALLER = SCRIPTS / "install.sh"

GIT_STUB = """#!/bin/sh
printf '%s\\n' "$*" >> "$GIT_LOG"
url="$4"
dir="$5"
[ -n "$GIT_SUCCEED" ] || exit 1
case "$url" in
    *"$GIT_SUCCEED"*) mkdir -p "$dir/.git"; exit 0 ;;
    *) exit 1 ;;
esac
"""

SSH_URL = "git@gitcode.com:GitHub_Trending/he/hermes-agent.git"
MIRROR_URL = "https://gitcode.com/GitHub_Trending/he/hermes-agent.git"
GITHUB_URL = "https://github.com/NousResearch/hermes-agent.git"


class CloneSourcesTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)

        self.bin = self.root / "bin"
        self.bin.mkdir()
        stub = self.bin / "git"
        stub.write_text(GIT_STUB, encoding="utf-8")
        stub.chmod(0o755)

        self.log = self.root / "git.log"
        self.install_dir = self.root / "install"

    def clone(self, succeeds, **env_overrides):
        """Source the installer and run clone_repo; return (returncode, git log)."""
        env = dict(os.environ)
        env["PATH"] = str(self.bin) + os.pathsep + env.get("PATH", "")
        env["HERMES_INSTALL_DIR"] = str(self.install_dir)
        env["GIT_LOG"] = str(self.log)
        env["GIT_SUCCEED"] = succeeds
        for key, value in env_overrides.items():
            env[key] = value

        result = subprocess.run(
            ["bash", "-c", 's="$1"; set --; source "$s"; clone_repo',
             "bash", str(INSTALLER)],
            capture_output=True, text=True, env=env)
        logged = self.log.read_text(encoding="utf-8") if self.log.exists() else ""
        return result, logged

    def test_mirror_failure_falls_back_to_the_github_upstream(self):
        result, logged = self.clone("github.com")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(GITHUB_URL, logged)
        self.assertTrue((self.install_dir / ".git").is_dir())

    def test_the_upstream_is_not_cloned_when_the_mirror_works(self):
        result, logged = self.clone("gitcode.com")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn(GITHUB_URL, logged,
                         "a working mirror must not be followed by a second clone")

    def test_every_source_is_reported_when_they_all_fail(self):
        result, logged = self.clone("nothing-matches-this")

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        for url in (SSH_URL, MIRROR_URL, GITHUB_URL):
            self.assertIn(url, logged, "each attempted source should be logged")
            self.assertIn(url, result.stdout,
                          "the failure message should name every source tried")

    def test_the_mirror_url_can_be_overridden(self):
        override = "https://mirror.example/hermes-agent.git"
        result, logged = self.clone("mirror.example",
                                    HERMES_REPO_URL_HTTPS=override)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(override, logged)
        self.assertNotIn(MIRROR_URL, logged)

    def test_the_upstream_url_can_be_overridden(self):
        override = "https://example.test/hermes-fork.git"
        result, logged = self.clone("example.test",
                                    HERMES_REPO_URL_GITHUB=override)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(override, logged)
        self.assertNotIn(GITHUB_URL, logged)

    def test_sourcing_does_not_run_the_installer(self):
        env = dict(os.environ)
        env["PATH"] = str(self.bin) + os.pathsep + env.get("PATH", "")
        env["HERMES_INSTALL_DIR"] = str(self.install_dir)
        env["GIT_LOG"] = str(self.log)

        result = subprocess.run(
            ["bash", "-c", 's="$1"; set --; source "$s"', "bash", str(INSTALLER)],
            capture_output=True, text=True, env=env)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.log.read_text(encoding="utf-8")
                         if self.log.exists() else "", "",
                         "sourcing must not clone anything")


if __name__ == "__main__":
    unittest.main()
