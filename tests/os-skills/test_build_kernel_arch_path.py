#!/usr/bin/env python3
"""Regression tests for the kernel-dev build script RPM paths (#3637).

Drives the real ``build-kernel.sh`` through a stub PATH (``uname``, ``rpm``,
``sudo``, ``grub2-mkconfig``, ``nproc``) and a temporary ``OUTPUT_DIR``, so the
SRPM install path can be exercised without root, a kernel source tree or an rpm
database.

Baseline on unchanged main: the two aarch64 tests fail. ``detect_system``
detects ``aarch64`` and ``rpmbuild`` writes packages under ``RPMS/aarch64/``,
but ``install_kernel`` globs the hardcoded ``RPMS/x86_64/kernel-*.rpm``. The
glob does not expand, the stub ``rpm`` is handed the literal non-existent
path, no package is installed, and the script still prints "SRPM kernel
installed" and exits 0. ``usage`` points the operator at the same empty
directory. The x86_64 control test passes before and after the fix.
"""

import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "src" / "os-skills" / "devops" / "kernel-dev" / "scripts" / "build-kernel.sh"

UNAME_STUB = """#!/bin/sh
case "$1" in
    -m) echo "${STUB_ARCH:-x86_64}" ;;
    -r) echo "6.6.102-1.stub" ;;
    *) exec uname "$@" ;;
esac
"""

RPM_STUB = """#!/bin/sh
printf '%s\\n' "$@" >> "$RPM_ARGV_LOG"
exit 0
"""

SUDO_STUB = """#!/bin/sh
exec "$@"
"""

NOOP_STUB = """#!/bin/sh
exit 0
"""

NPROC_STUB = """#!/bin/sh
echo 4
"""


class BuildKernelArchPathTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="build-kernel-arch-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.bin_dir = self.tmp / "bin"
        self.bin_dir.mkdir(parents=True)
        self.output_dir = self.tmp / "rpmbuild"
        self.rpm_argv_log = self.tmp / "rpm-argv.log"

        for name, body in (
            ("uname", UNAME_STUB),
            ("rpm", RPM_STUB),
            ("sudo", SUDO_STUB),
            ("grub2-mkconfig", NOOP_STUB),
            ("nproc", NPROC_STUB),
        ):
            self.write_stub(name, body)

        # Sandbox the three machine-wide defaults the script hardcodes, leaving
        # every other line — including the paths under test — untouched.
        source = SCRIPT.read_text(encoding="utf-8")
        for original, replacement in (
            ('OUTPUT_DIR="/root/rpmbuild"', f'OUTPUT_DIR="{self.output_dir}"'),
            ('WORK_DIR="/root/upstream-kernel"', f'WORK_DIR="{self.tmp}/upstream-kernel"'),
            ('LOG_FILE="/tmp/kernel-build.log"', f'LOG_FILE="{self.tmp}/kernel-build.log"'),
        ):
            self.assertEqual(source.count(original), 1, f"sandbox anchor moved: {original}")
            source = source.replace(original, replacement)

        self.script = self.tmp / "build-kernel.sh"
        self.script.write_text(source, encoding="utf-8")
        self.script.chmod(0o755)

    def write_stub(self, name, body):
        path = self.bin_dir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def env(self, arch):
        env = dict(os.environ)
        env["PATH"] = f"{self.bin_dir}{os.pathsep}{env.get('PATH', '')}"
        env["STUB_ARCH"] = arch
        env["RPM_ARGV_LOG"] = str(self.rpm_argv_log)
        env["HOME"] = str(self.tmp)
        return env

    def run_script(self, arch, *args):
        return subprocess.run(
            ["bash", str(self.script), *args],
            cwd=self.tmp,
            env=self.env(arch),
            capture_output=True,
            text=True,
            timeout=120,
        )

    def make_package(self, arch, name):
        pkg_dir = self.output_dir / "RPMS" / arch
        pkg_dir.mkdir(parents=True, exist_ok=True)
        package = pkg_dir / name
        package.write_bytes(b"stub rpm payload")
        return package

    def rpm_argv(self):
        self.assertTrue(self.rpm_argv_log.exists(), "the rpm stub was never invoked")
        return self.rpm_argv_log.read_text(encoding="utf-8").splitlines()


class TestSrpmInstallUsesDetectedArch(BuildKernelArchPathTestCase):
    def test_aarch64_install_passes_the_built_package_to_rpm(self):
        package = self.make_package("aarch64", "kernel-6.6.102-1.aarch64.rpm")

        result = self.run_script("aarch64", "install", "srpm")

        self.assertEqual(result.returncode, 0, result.stderr)
        argv = self.rpm_argv()
        self.assertIn(str(package), argv)
        rpm_paths = [a for a in argv if a.endswith(".rpm")]
        self.assertEqual(rpm_paths, [str(package)])
        for path in rpm_paths:
            self.assertTrue(Path(path).exists(), f"rpm was handed a non-existent path: {path}")
        self.assertNotIn("x86_64", "\n".join(argv))

    def test_x86_64_install_still_passes_the_built_package_to_rpm(self):
        package = self.make_package("x86_64", "kernel-6.6.102-1.x86_64.rpm")

        result = self.run_script("x86_64", "install", "srpm")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([a for a in self.rpm_argv() if a.endswith(".rpm")], [str(package)])

    def test_aarch64_reports_an_rpms_location_that_exists(self):
        self.make_package("aarch64", "kernel-6.6.102-1.aarch64.rpm")

        result = self.run_script("aarch64", "help")

        self.assertEqual(result.returncode, 0, result.stderr)
        reported = [line for line in result.stdout.splitlines() if "SRPM builds:" in line]
        self.assertEqual(len(reported), 1, result.stdout)
        self.assertIn(f"{self.output_dir}/RPMS/aarch64/", reported[0])
        self.assertNotIn("x86_64", reported[0])


class TestNoHardcodedArchRemains(BuildKernelArchPathTestCase):
    def test_script_does_not_hardcode_the_x86_64_rpms_path(self):
        offenders = [
            f"build-kernel.sh:{lineno}: {line.strip()}"
            for lineno, line in enumerate(
                SCRIPT.read_text(encoding="utf-8").splitlines(), start=1
            )
            if "RPMS/x86_64" in line
        ]
        self.assertEqual(offenders, [], "hardcoded x86_64 RPM path still present")


if __name__ == "__main__":
    unittest.main()
