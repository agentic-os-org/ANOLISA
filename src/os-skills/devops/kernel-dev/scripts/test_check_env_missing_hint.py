#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for the missing-dependency summary in check-env.sh.

Regression test for #5505: the absent kernel build directory was appended to the
same MISSING array as the packages, so the remediation the script printed was a
single `sudo yum install -y … kernel-build-dir …`. On a dnf-based host one
unknown name fails the whole transaction ("No match for argument"), so none of
the genuinely missing packages were installed either.

`uname` and `rpm` are stubbed on PATH, so the cases need neither an Alinux host
nor root: the check runs on any machine and the build directory is always absent
for the fake kernel release.

Usage: python3 src/os-skills/devops/kernel-dev/scripts/test_check_env_missing_hint.py
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-env.sh")
FAKE_KERNEL = "9.9.9-fake"

UNAME_STUB = """#!/bin/sh
case "$1" in
  -m) echo x86_64 ;;
  -r) echo %s ;;
  *) echo Linux ;;
esac
""" % FAKE_KERNEL

# Installed, unless the queried package is listed in RPM_MISSING.
RPM_STUB = """#!/bin/sh
for arg in "$@"; do
  case "$arg" in -*) continue ;; esac
  case " $RPM_MISSING " in *" $arg "*) exit 1 ;; esac
done
case "$*" in *--qf*) echo "1.0" ;; esac
exit 0
"""


class CheckEnvMissingHintTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.stubs = self.root / "bin"
        self.stubs.mkdir()
        for name, body in (("uname", UNAME_STUB), ("rpm", RPM_STUB)):
            path = self.stubs / name
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)

    def run_check(self, missing=""):
        env = dict(os.environ)
        env["RPM_MISSING"] = missing
        env["PATH"] = f"{self.stubs}:{env['PATH']}"
        result = subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True)
        return result, result.stdout + result.stderr

    @staticmethod
    def install_lines(output):
        return [line.strip() for line in output.splitlines()
                if line.strip().startswith("sudo yum install -y")]

    def test_the_install_hint_lists_only_real_packages(self):
        result, output = self.run_check(missing="gcc make")
        self.assertEqual(result.returncode, 1, "a host missing packages is not ready")

        lines = self.install_lines(output)
        self.assertTrue(lines, "the summary must still tell the operator what to install")
        joined = " ".join(lines)
        self.assertIn("gcc", joined)
        self.assertIn("make", joined)
        self.assertNotIn("kernel-build-dir", output,
                         "a pseudo-package in the command makes dnf fail the whole transaction")

    def test_a_missing_build_directory_does_not_masquerade_as_a_package(self):
        result, output = self.run_check(missing="")
        self.assertEqual(result.returncode, 1,
                         "an absent build directory is not a ready environment either")

        self.assertNotIn("kernel-build-dir", output)
        self.assertIn(f"kernel-devel-{FAKE_KERNEL}", output,
                      "the hint must name the package that creates the directory")
        for line in self.install_lines(output):
            self.assertNotIn("gcc", line,
                             "with every package installed there is nothing else to install")

    def test_a_package_and_the_directory_are_reported_separately(self):
        result, output = self.run_check(missing="gcc")
        self.assertEqual(result.returncode, 1)

        lines = self.install_lines(output)
        self.assertIn("sudo yum install -y gcc", lines)
        # The -devel package reports itself as installed while the directory it
        # should have created is absent, so the directory needs its own line.
        self.assertIn(f"sudo yum install -y kernel-devel-{FAKE_KERNEL}", lines)
        self.assertNotIn("kernel-build-dir", output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
