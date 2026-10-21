#!/usr/bin/env python3
"""Regression tests for install_sys_deps in install-claude-code.sh (#3865).

The installer runs under ``set -euo pipefail`` and probed
``rpm -q glibc`` / ``rpm -q libstdc++`` unconditionally before calling
``sudo dnf install``. On a host with neither ``rpm`` nor ``dnf`` (macOS,
Debian family, slim containers) the failed probes fabricated a
``glibc libstdc++`` package list and the script died with exit 127
(``dnf: command not found``) in Step 1, so the documented install chain
(native -> npm -> nvm) was never attempted.

The tests extract the real functions from the shipped script and drive them
in a bash harness with a mocked PATH: no network, no package manager, no
writes outside a temp dir.

Baseline on unchanged main: the no-rpm/no-dnf and rpm-without-dnf cases fail
with exit 127 and never reach the line after ``install_sys_deps``; the two
RPM-host controls pass.
"""

import os
import re
import shutil
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

BASH = shutil.which("bash") or "/bin/bash"
COLOR_VARS = ("RED", "GREEN", "YELLOW", "BLUE", "NC")
NEEDED_FUNCTIONS = ("info", "ok", "warn", "err", "command_exists", "install_sys_deps")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def extract_function(source_lines, name):
    """Return the ``name() { ... }`` block verbatim from the script."""
    start = None
    for index, line in enumerate(source_lines):
        if re.match(r"^%s\(\)\s*\{" % re.escape(name), line):
            start = index
            break
    if start is None:
        raise AssertionError("function %s not found in %s" % (name, SCRIPT))
    if source_lines[start].rstrip().endswith("}"):
        return [source_lines[start]]
    block = [source_lines[start]]
    for line in source_lines[start + 1:]:
        block.append(line)
        if line.rstrip() == "}":
            return block
    raise AssertionError("unterminated function %s" % name)


def build_harness():
    """Assemble a bash script holding only the functions under test."""
    source_lines = SCRIPT.read_text(encoding="utf-8").splitlines(keepends=True)
    parts = ["set -euo pipefail\n"]
    for line in source_lines:
        if line.split("=", 1)[0].strip("'\" ") in COLOR_VARS and "=" in line:
            parts.append(line)
    for name in NEEDED_FUNCTIONS:
        parts.extend(extract_function(source_lines, name))
    parts.append("install_sys_deps\n")
    parts.append('echo "REACHED_END"\n')
    return "".join(parts)


def write_mock(directory, name, body):
    path = directory / name
    path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
    path.chmod(0o755)
    return path


class InstallSysDepsTestCase(unittest.TestCase):
    """Runs the real install_sys_deps against a mocked PATH."""

    @classmethod
    def setUpClass(cls):
        cls.harness = build_harness()

    def setUp(self):
        self.workdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.workdir.cleanup)
        root = Path(self.workdir.name)
        self.bin_dir = root / "bin"
        self.log_dir = root / "log"
        self.bin_dir.mkdir()
        self.log_dir.mkdir()
        self.script_path = root / "harness.sh"
        self.script_path.write_text(self.harness, encoding="utf-8")
        # Present-but-irrelevant tools: install_sys_deps only probes them.
        for tool in ("curl", "git", "tar", "gzip"):
            write_mock(self.bin_dir, tool, "exit 0\n")
        write_mock(
            self.bin_dir,
            "sudo",
            'printf "%s\\n" "$*" >> "$MOCK_LOG_DIR/sudo.args"\nexec "$@"\n',
        )
        write_mock(
            self.bin_dir,
            "dnf",
            'printf "%s\\n" "$*" >> "$MOCK_LOG_DIR/dnf.args"\nexit 0\n',
        )

    def add_rpm_mock(self, installed=()):
        listing = " ".join(installed)
        write_mock(
            self.bin_dir,
            "rpm",
            'pkg="$2"\nfor have in %s; do\n'
            '  [ "$pkg" = "$have" ] && exit 0\ndone\nexit 1\n' % listing,
        )

    def drop_mock(self, *names):
        for name in names:
            (self.bin_dir / name).unlink()

    def run_harness(self):
        env = {
            "PATH": str(self.bin_dir),
            "HOME": str(self.log_dir),
            "MOCK_LOG_DIR": str(self.log_dir),
        }
        proc = subprocess.run(
            [BASH, str(self.script_path)],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        return proc, ANSI_RE.sub("", proc.stdout + proc.stderr)

    def logged(self, name):
        path = self.log_dir / name
        return path.read_text(encoding="utf-8") if path.exists() else ""


class TestNonRpmHosts(InstallSysDepsTestCase):
    def test_no_rpm_no_dnf_skips_packages_and_continues(self):
        self.drop_mock("dnf")
        proc, out = self.run_harness()
        self.assertEqual(0, proc.returncode, out)
        self.assertIn("REACHED_END", out)
        self.assertIn("skipping distribution packages", out)
        self.assertEqual("", self.logged("sudo.args"))

    def test_rpm_without_dnf_skips_instead_of_dying(self):
        self.drop_mock("dnf")
        self.add_rpm_mock(installed=())
        proc, out = self.run_harness()
        self.assertEqual(0, proc.returncode, out)
        self.assertIn("REACHED_END", out)
        self.assertIn("dnf not found", out)
        self.assertIn("glibc", out)
        self.assertEqual("", self.logged("sudo.args"))


class TestRpmHosts(InstallSysDepsTestCase):
    def test_complete_rpm_host_installs_nothing(self):
        self.add_rpm_mock(installed=("glibc", "libstdc++"))
        proc, out = self.run_harness()
        self.assertEqual(0, proc.returncode, out)
        self.assertIn("All system prerequisites already installed.", out)
        self.assertEqual("", self.logged("dnf.args"))

    def test_missing_rpm_package_is_installed_via_dnf(self):
        self.add_rpm_mock(installed=("libstdc++",))
        proc, out = self.run_harness()
        self.assertEqual(0, proc.returncode, out)
        self.assertIn("System prerequisites installed.", out)
        args = self.logged("sudo.args")
        self.assertIn("install -y glibc", args)
        self.assertNotIn("libstdc++", args)


if __name__ == "__main__":
    unittest.main()
