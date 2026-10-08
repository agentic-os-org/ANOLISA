#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for verify-env.sh os-release PRETTY_NAME parsing.

verify-env.sh extracts PRETTY_NAME with `cut -d'"' -f2`, which only works
for double-quoted values. os-release(5) also allows single-quoted and
unquoted values: for those the cut returns the whole raw line, so the
summary prints "OS: PRETTY_NAME=Alinux 4 Test" (or with embedded quotes)
instead of the pretty name. The value must be taken from the first '='
onwards and optional surrounding quotes stripped.

The grep/rpm/uname/gcc/make commands are stubbed into a private fixture
PATH (grep answers from fixture os-release content; uname -r redirects
the build-path probe into the fixture via dot-dot traversal), so no
system file or package is touched.

Baseline on unchanged main: 2 failures / 1 passing control.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "devops"
    / "kernel-dev"
    / "scripts"
    / "verify-env.sh"
)

UNQUOTED = "PRETTY_NAME=Alinux 4 Test"
SINGLE = "PRETTY_NAME='Alinux 4 Test'"
DOUBLE = 'PRETTY_NAME="Alinux 4 Test"'


class VerifyEnvPrettyNameTests(unittest.TestCase):
    def setUp(self):
        if not os.path.isdir("/lib/modules"):
            raise unittest.SkipTest("/lib/modules not present on this host")
        if not Path("/bin/bash").exists():
            raise unittest.SkipTest("/bin/bash not present on this host")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.stubs = root / "stubs"
        self.stubs.mkdir()
        self.fixture_root = root / "fixture"
        self.fixture_root.mkdir()
        # Relative path from the physical /lib/modules so the script's
        # hardcoded "/lib/modules/$KERNEL_VER/build" resolves inside the
        # fixture (usrmerge /lib symlink handled by realpath).
        modules_base = os.path.realpath("/lib/modules")
        self.modules_rel = os.path.relpath(self.fixture_root, modules_base)
        self.build = self.fixture_root / "pretty-name" / "build"
        self.build.mkdir(parents=True)
        (self.build / "Makefile").write_text("obj-y :=\n", encoding="utf-8")

    def write_stub(self, name, text):
        path = self.stubs / name
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def run_verify_env(self, pretty_name_line):
        # grep: the -qi alinux probe succeeds; PRETTY_NAME queries answer
        # with fixture os-release content regardless of quoting.
        self.write_stub(
            "grep",
            "#!/bin/sh\ncase \" $* \" in\n"
            "  *' -q'*) exit 0 ;;\n"
            "  *PRETTY_NAME*) echo '" + pretty_name_line + "' ;;\n"
            "esac\nexit 1\n",
        )
        self.write_stub(
            "uname",
            "#!/bin/sh\ncase \"$1\" in\n"
            "  -m) echo x86_64 ;;\n"
            f"  -r) echo '{self.modules_rel}/pretty-name' ;;\n"
            "  *) /usr/bin/uname \"$@\" ;;\n"
            "esac\n",
        )
        self.write_stub("rpm", "#!/bin/sh\necho fixture-package\n")
        self.write_stub("gcc", "#!/bin/sh\necho 'gcc (fixture) 11.4.0'\n")
        # make "builds" the fixture module so the test-compilation step passes.
        self.write_stub("make", "#!/bin/sh\ntouch test_module.ko\nexit 0\n")
        self.write_stub("kmod", "#!/bin/sh\necho 'kmod version 30'\n")
        self.write_stub("ls", "#!/bin/sh\necho /usr/include/linux/fixture.h\n")
        # Stubs come first so the probes are deterministic; real system
        # tools (mktemp, cat, cut, head, wc, readlink) stay available.
        env = dict(os.environ)
        env["PATH"] = f"{self.stubs}:/usr/bin:/bin"
        result = subprocess.run(
            ["/bin/bash", str(SCRIPT)],
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
        )
        return result

    def assert_os_line(self, result, pretty_name_line):
        self.assertEqual(
            result.returncode, 0, result.stdout[-600:] + result.stderr[-300:]
        )
        self.assertIn("OS: Alinux 4 Test", result.stdout, result.stdout[-600:])
        self.assertNotIn("PRETTY_NAME=", result.stdout, result.stdout[-600:])

    def test_unquoted_pretty_name_is_reported(self):
        result = self.run_verify_env(UNQUOTED)
        self.assert_os_line(result, UNQUOTED)

    def test_single_quoted_pretty_name_is_reported(self):
        result = self.run_verify_env(SINGLE)
        self.assert_os_line(result, SINGLE)

    def test_double_quoted_pretty_name_still_reported(self):
        """Control: the previously working quoting style is unchanged."""
        result = self.run_verify_env(DOUBLE)
        self.assert_os_line(result, DOUBLE)


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
