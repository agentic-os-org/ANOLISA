#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for check-env.sh compiler status contribution.

Regression test: the Compiler Information block printed a red "gcc not
found" line when gcc was not on the caller's PATH but never appended to
MISSING, so with the RPM checks passing and the build directory present
the summary reported "All dependencies are installed" and exited 0 -
certifying an environment whose compiler is unreachable. The PATH probe
must contribute to the failure status like every other check.

The rpm/uname/gcc commands are stubbed into a private fixture PATH and
the inspected build path is redirected by stubbing `uname -r` to a
dot-dot traversal relative to /lib/modules; no system file, package or
compilation is touched.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-env.sh")

RPM_OK_STUB = "#!/bin/sh\necho fixture-package\n"
RPM_FAIL_STUB = "#!/bin/sh\nexit 1\n"
GCC_STUB = "#!/bin/sh\necho 'gcc (fixture) 11.4.0'\n"


class CheckEnvGccStatusTests(unittest.TestCase):
    def setUp(self):
        if not os.path.isdir("/lib/modules"):
            raise unittest.SkipTest(
                "/lib/modules not present on this host (non-Linux)"
            )
        if not Path("/bin/bash").exists():
            raise unittest.SkipTest("/bin/bash not present on this host")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.stubs = root / "stubs"
        self.stubs.mkdir()
        self.fixture_root = root / "fixture"
        self.fixture_root.mkdir()
        # Relative path from the physical location of /lib/modules so the
        # script's hardcoded "/lib/modules/$KERNEL_VER/build" resolves
        # inside the fixture (on usrmerge systems /lib is a symlink to
        # usr/lib, and ".." resolves from the physical directory).
        modules_base = os.path.realpath("/lib/modules")
        self.modules_rel = os.path.relpath(self.fixture_root, modules_base)
        for name in ("grep", "cut", "head", "readlink"):
            (self.stubs / name).symlink_to(Path("/usr/bin") / name)
        self.build = self.fixture_root / "gcc-status" / "build"
        self.build.mkdir(parents=True)
        (self.build / "Makefile").write_text("obj-y :=\n", encoding="utf-8")

    def write_stub(self, name, text):
        path = self.stubs / name
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def run_check_env(self, rpm_ok, gcc_on_path):
        self.write_stub(
            "uname",
            "#!/bin/sh\ncase \"$1\" in\n"
            "  -m) echo x86_64 ;;\n"
            f"  -r) echo '{self.modules_rel}/gcc-status' ;;\n"
            "  *) /usr/bin/uname \"$@\" ;;\n"
            "esac\n",
        )
        self.write_stub("rpm", RPM_OK_STUB if rpm_ok else RPM_FAIL_STUB)
        if gcc_on_path:
            self.write_stub("gcc", GCC_STUB)
        # A pure stub PATH: without system entries, `command -v gcc` can
        # only see the fixture stub, so gcc-absent cases cannot leak the
        # host compiler.
        env = dict(os.environ)
        env["PATH"] = str(self.stubs)
        result = subprocess.run(
            ["/bin/bash", str(SCRIPT)],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        plain = result.stdout.replace("\x1b[0m", "")
        return result, plain

    @staticmethod
    def gcc_missing_entries(plain):
        return [line for line in plain.splitlines() if line.strip() == "- gcc"]

    # --- regression: an unreachable compiler must fail readiness ---

    def test_off_path_gcc_fails_readiness(self):
        """rpm says gcc is installed but PATH cannot reach it."""
        result, plain = self.run_check_env(rpm_ok=True, gcc_on_path=False)
        self.assertIn("gcc not found", plain)
        self.assertNotIn("You can now compile kernel modules.", plain)
        self.assertEqual(result.returncode, 1, plain)
        self.assertEqual(len(self.gcc_missing_entries(plain)), 1, plain)

    # --- controls: the other three combinations keep their status ---

    def test_present_gcc_and_installed_package_pass(self):
        result, plain = self.run_check_env(rpm_ok=True, gcc_on_path=True)
        self.assertEqual(result.returncode, 0, plain)
        self.assertIn("All dependencies are installed", plain)
        self.assertEqual(self.gcc_missing_entries(plain), [])

    def test_missing_package_gcc_listed_once_with_compiler_present(self):
        """The RPM loop already lists gcc; the PATH probe must not repeat it."""
        result, plain = self.run_check_env(rpm_ok=False, gcc_on_path=True)
        self.assertEqual(result.returncode, 1, plain)
        self.assertEqual(len(self.gcc_missing_entries(plain)), 1, plain)

    def test_missing_package_gcc_listed_once_without_compiler(self):
        """Both probes fail; the summary lists gcc exactly once."""
        result, plain = self.run_check_env(rpm_ok=False, gcc_on_path=False)
        self.assertEqual(result.returncode, 1, plain)
        self.assertEqual(len(self.gcc_missing_entries(plain)), 1, plain)


if __name__ == "__main__":
    unittest.main()
