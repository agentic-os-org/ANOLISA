#!/usr/bin/env python3
"""Regression tests for kernel-dev test-module.sh parameter passing (#3744).

The script joined its parameter arguments into a string and re-expanded that
string unquoted into ``insmod``, so a quoted value containing a space was
word-split into two arguments (the kernel rejects the trailing word as an
unknown parameter) and a value containing a glob was expanded against the
current directory.

These tests drive the real script through a stubbed toolchain
(insmod/lsmod/rmmod/modinfo/dmesg/sleep) on a sandboxed PATH, so they need no
root, no kernel module support and no network. Only the root check is
neutralised in the temporary copy; the argument handling under test is
byte-identical to the shipped script.

Baseline on unchanged main: 3 failures (split value, re-globbed value,
displayed parameter line) / 1 passing control (parameterless invocation).
"""

import os
import re
import shutil
import subprocess
import sys
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
    / "test-module.sh"
)

MODULE_NAME = "mymod"

# The shipped script refuses to run unprivileged; the tests stub every
# privileged command, so only this guard has to be lifted in the temp copy.
ROOT_CHECK = re.compile(r'^if \[ "\$EUID" -ne 0 \]; then[ \t]*$', re.MULTILINE)


@unittest.skipIf(shutil.which("bash") is None, "bash is required")
class TestModuleParameterPassing(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name).resolve()
        self.addCleanup(self._tmp.cleanup)
        self._build_sandbox()

    # --- sandbox ---------------------------------------------------------
    def _build_sandbox(self):
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        self.capture = self.tmp / "insmod-args"
        self.loaded = self.tmp / "loaded"
        self.script = self.tmp / "test-module.sh"

        stubs = {
            # insmod records every parameter NUL-separated and only accepts
            # key=value words, like the kernel's module parameter parser.
            "insmod": (
                "#!/bin/bash\n"
                f': >> "{self.capture}"  # record the call even with no params\n'
                'shift  # the module file\n'
                'for arg in "$@"; do\n'
                '    case "$arg" in\n'
                "        *=*) ;;\n"
                '        *) echo "insmod: unknown parameter \'$arg\'" >&2; exit 1 ;;\n'
                "    esac\n"
                "done\n"
                'for arg in "$@"; do printf \'%s\\0\' "$arg" >> '
                f'"{self.capture}"; done\n'
                f': > "{self.loaded}"\n'
            ),
            "lsmod": (
                "#!/bin/bash\n"
                f'if [ -f "{self.loaded}" ]; then\n'
                f"    printf '{MODULE_NAME} 16384 0 - Live\\n'\n"
                "fi\n"
            ),
            "rmmod": "#!/bin/bash\n" f'rm -f "{self.loaded}"\n',
            "modinfo": "#!/bin/bash\nexit 0\n",
            "dmesg": "#!/bin/bash\nexit 0\n",
            "sleep": "#!/bin/bash\nexit 0\n",
        }
        for name, body in stubs.items():
            stub = bin_dir / name
            stub.write_text(body, encoding="utf-8")
            stub.chmod(0o755)

        source = SCRIPT.read_text(encoding="utf-8")
        unprivileged, count = ROOT_CHECK.subn("if false; then", source)
        self.assertEqual(
            count, 1, "root check not found in test-module.sh; update ROOT_CHECK"
        )
        self.script.write_text(unprivileged, encoding="utf-8")
        self.script.chmod(0o755)

        # The module file the script looks for next to itself.
        (self.tmp / f"{MODULE_NAME}.ko").write_bytes(b"")

    def _run(self, params):
        env = dict(os.environ)
        env["PATH"] = f"{self.tmp / 'bin'}{os.pathsep}{env.get('PATH', '/usr/bin:/bin')}"
        return subprocess.run(
            ["bash", str(self.script), MODULE_NAME, *params],
            cwd=self.tmp,
            env=env,
            capture_output=True,
            text=True,
        )

    def _insmod_params(self):
        if not self.capture.exists():
            return None
        raw = self.capture.read_bytes().decode("utf-8", "replace")
        if not raw:
            return []
        return raw.split("\0")[:-1]

    def _report(self, proc):
        return f"exit={proc.returncode}\n--- stdout ---\n{proc.stdout}--- stderr ---\n{proc.stderr}"

    # --- tests -----------------------------------------------------------
    def test_quoted_value_with_space_reaches_insmod_as_one_argument(self):
        params = ["greeting=Hello World", "repeat_count=3"]
        proc = self._run(params)
        self.assertEqual(self._insmod_params(), params, self._report(proc))
        self.assertEqual(proc.returncode, 0, self._report(proc))
        self.assertIn("All tests passed", proc.stdout)

    def test_glob_value_is_not_expanded_against_the_cwd(self):
        # A matching file in the cwd makes the buggy re-globbing deterministic.
        (self.tmp / "pattern=glob-marker").write_bytes(b"")
        (self.tmp / "other.ko").write_bytes(b"")
        params = ["pattern=*", "firmware=*.ko"]
        proc = self._run(params)
        self.assertEqual(self._insmod_params(), params, self._report(proc))
        self.assertEqual(proc.returncode, 0, self._report(proc))

    def test_parameters_are_reported_verbatim(self):
        proc = self._run(["greeting=Hello World", "pattern=*"])
        self.assertEqual(proc.returncode, 0, self._report(proc))
        self.assertIn(
            "Loading with parameters: greeting=Hello World pattern=*", proc.stdout
        )

    def test_parameterless_invocation_is_unchanged(self):
        proc = self._run([])
        self.assertEqual(proc.returncode, 0, self._report(proc))
        self.assertEqual(self._insmod_params(), [], self._report(proc))
        self.assertIn("Module loaded", proc.stdout)
        self.assertNotIn("Params:", proc.stdout)
        self.assertIn("All tests passed", proc.stdout)


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
