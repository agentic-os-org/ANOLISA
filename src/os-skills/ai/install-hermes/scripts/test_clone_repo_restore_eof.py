#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for the restore prompt in install.sh clone_repo().

Regression test for #5507: after stashing local changes and fast-forwarding the
checkout, the update path asked "Restore local changes now? [Y/n]" with a bare
`read -r restore_answer` while the installer runs under `set -e`. EOF at that
prompt — Ctrl-D, or the terminal going away — returned non-zero, so errexit
aborted the whole installer right after the update had succeeded: the stash
stayed unrestored with no guidance, and the symlink, config-template, PATH and
tokenless-plugin steps never ran. Every other prompt in this file guards its
read the same way (`prompt_yes_no`, lines 145/148); this one did not.

The scenario drives the real `clone_repo()` through a pseudo-terminal, because
the prompt only exists when stdin and stdout are both ttys, and then delivers
Ctrl-D. `git stash drop` is neutralised through a PATH wrapper: passing the
recorded object id to it is a separate defect (#5498) that would abort the run
for an unrelated reason.

Usage: python3 src/os-skills/ai/install-hermes/scripts/test_clone_repo_restore_eof.py
"""

import os
import pty
import select
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("install.sh")
PROMPT = "Restore local changes now?"
LOCAL_CHANGE = "local customization\n"

# Everything passes through to the real git except `git stash drop`, which is
# out of scope here (see #5498).
GIT_WRAPPER = """#!/bin/sh
if [ "$1" = "stash" ] && [ "$2" = "drop" ]; then
  exit 0
fi
exec "%s" "$@"
"""


def git(cwd, *args, env=None, check=True):
    return subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=check,
        env=env,
    )


class RestorePromptEofTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.git_env = dict(
            os.environ,
            GIT_AUTHOR_NAME="audit",
            GIT_AUTHOR_EMAIL="audit@example.com",
            GIT_COMMITTER_NAME="audit",
            GIT_COMMITTER_EMAIL="audit@example.com",
        )

        # The installer's git calls go through the wrapper.
        self.stubs = self.root / "bin"
        self.stubs.mkdir()
        wrapper = self.stubs / "git"
        wrapper.write_text(GIT_WRAPPER % shutil.which("git"), encoding="utf-8")
        wrapper.chmod(0o755)

        # A remote with one commit, a checkout that carries an uncommitted
        # change, and a remote that has moved ahead: the update path then has
        # something to stash and something to fast-forward to.
        self.remote = self.root / "remote.git"
        subprocess.run(["git", "init", "--bare", "-q", "-b", "main", str(self.remote)],
                       check=True, capture_output=True)
        seed = self.root / "seed"
        git(self.root, "clone", "-q", str(self.remote), str(seed), env=self.git_env)
        git(seed, "checkout", "-q", "-B", "main", env=self.git_env)
        (seed / "file.txt").write_text("base\n", encoding="utf-8")
        git(seed, "add", ".", env=self.git_env)
        git(seed, "commit", "-q", "-m", "base", env=self.git_env)
        git(seed, "push", "-q", "origin", "main", env=self.git_env)

        self.install_dir = self.root / "install"
        git(self.root, "clone", "-q", str(self.remote), str(self.install_dir), env=self.git_env)
        git(self.install_dir, "checkout", "-q", "-B", "main", env=self.git_env)
        (self.install_dir / "file.txt").write_text(LOCAL_CHANGE, encoding="utf-8")

        (seed / "file2.txt").write_text("new\n", encoding="utf-8")
        git(seed, "add", ".", env=self.git_env)
        git(seed, "commit", "-q", "-m", "ahead", env=self.git_env)
        git(seed, "push", "-q", env=self.git_env)

    # --- harness ---------------------------------------------------------

    def scenario_script(self):
        """The installer without its trailing `main`, plus only clone_repo()."""
        body = SCRIPT.read_text(encoding="utf-8").rstrip()
        self.assertTrue(body.endswith("main"), "the installer must end in a bare main invocation")
        body = body[: -len("main")]
        body += (
            f'\nINSTALL_DIR="{self.install_dir}"\n'
            'BRANCH="main"\n'
            "clone_repo\n"
            'echo "HARNESS-COMPLETED"\n'
        )
        path = self.root / "scenario.sh"
        path.write_text(body, encoding="utf-8")
        return path

    def run_scenario_with_eof(self, timeout=60):
        """Run under a pty and send Ctrl-D once the prompt is on screen."""
        script = self.scenario_script()
        env = dict(os.environ, PATH=f"{self.stubs}:{os.environ['PATH']}")
        master, slave = pty.openpty()
        try:
            process = subprocess.Popen(
                ["bash", str(script)], stdin=slave, stdout=slave, stderr=slave,
                close_fds=True, env=env, cwd=str(self.root),
            )
        finally:
            os.close(slave)

        output = b""
        sent_eof = False
        deadline = time.time() + timeout
        while time.time() < deadline:
            readable, _, _ = select.select([master], [], [], 0.5)
            if readable:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
                if not sent_eof and PROMPT in output.decode("utf-8", "replace"):
                    # Ctrl-D on an empty canonical line is EOF for the pending read.
                    os.write(master, b"\x04")
                    sent_eof = True
            if process.poll() is not None:
                break
        try:
            returncode = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            returncode = -1
        os.close(master)
        return returncode, output.decode("utf-8", "replace"), sent_eof

    # --- the reported defect --------------------------------------------

    def test_eof_at_the_restore_prompt_does_not_abort_the_install(self):
        returncode, output, sent_eof = self.run_scenario_with_eof()

        self.assertTrue(sent_eof, f"the prompt never appeared:\n{output}")
        self.assertIn("HARNESS-COMPLETED", output,
                      "EOF at the prompt must fall through to the default, not abort via errexit")
        self.assertEqual(returncode, 0, f"installer exited {returncode}:\n{output}")

    def test_eof_restores_the_stash_like_the_documented_default(self):
        _, output, sent_eof = self.run_scenario_with_eof()
        self.assertTrue(sent_eof, f"the prompt never appeared:\n{output}")

        restored = (self.install_dir / "file.txt").read_text(encoding="utf-8")
        self.assertEqual(restored, LOCAL_CHANGE,
                         "the empty answer is documented as 'restore now', so the change must be back")
        self.assertIn("Local changes were restored", output)
        self.assertNotIn("restoring local changes failed", output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
