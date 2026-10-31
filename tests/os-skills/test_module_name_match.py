#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for test-module.sh lsmod name matching.

test-module.sh greps `lsmod` output with an unanchored-suffix pattern
(`grep -q "^${MODULE_NAME}"`), so a *different* loaded module whose name
starts with the requested name also matches: the suite reports "already
loaded" for a module that is not loaded, and after a successful unload
it still sees the longer-named module and exits 1 with "failed to
unload". Matching must anchor on the full first column of lsmod output
(module name followed by whitespace).

The lsmod/insmod/rmmod/modinfo/dmesg/sleep commands are stubbed into a
private fixture PATH; lsmod is stateful through a marker file so the
load/unload steps behave like the real kernel module list. The script
runs from a copied tree with a fixture examples/ layout; no kernel
module is actually loaded.

Baseline on unchanged main: 1 failure / 1 passing control.
"""

import os
import shutil
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
    / "test-module.sh"
)


class TestModuleNameMatch(unittest.TestCase):
    def setUp(self):
        if os.geteuid() != 0:
            raise unittest.SkipTest("test-module.sh refuses to run as non-root")
        if not Path("/bin/bash").exists():
            raise unittest.SkipTest("/bin/bash not present on this host")
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)

        # Copied script tree: BASE_DIR/examples/<mod>/<mod>.ko is the first
        # search location the script probes.
        self.tree = root / "tree"
        (self.tree / "scripts").mkdir(parents=True)
        shutil.copy2(SCRIPT, self.tree / "scripts" / "test-module.sh")
        (self.tree / "examples" / "hello_module").mkdir(parents=True)
        (self.tree / "examples" / "hello_module" / "hello_module.ko").write_bytes(
            b"fake ko\n"
        )

        self.marker = root / "loaded.marker"
        self.stubs = root / "stubs"
        self.stubs.mkdir()
        self.base_line = "other_mod 16384 0"
        self.marker_line = "hello_module 16384 0"

    def write_stub(self, name, text):
        path = self.stubs / name
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def run_suite(self, module_name, pre_loaded=False):
        # Stateful lsmod: the base line is always present; the module under
        # test appears only while the insmod marker file exists.
        self.write_stub(
            "lsmod",
            "#!/bin/sh\n"
            f'echo "{self.base_line}"\n'
            'if [ -f "$LSMOD_MARKER" ]; then\n'
            f'  echo "{self.marker_line}"\n'
            "fi\n",
        )
        self.write_stub("insmod", '#!/bin/sh\ntouch "$LSMOD_MARKER"\nexit 0\n')
        self.write_stub("rmmod", '#!/bin/sh\nrm -f "$LSMOD_MARKER"\nexit 0\n')
        self.write_stub("modinfo", "#!/bin/sh\necho 'filename: fixture.ko'\nexit 0\n")
        self.write_stub("dmesg", "#!/bin/sh\nexit 0\n")
        self.write_stub("sleep", "#!/bin/sh\nexit 0\n")

        env = dict(os.environ)
        env["PATH"] = f"{self.stubs}:{env.get('PATH', '')}"
        env["LSMOD_MARKER"] = str(self.marker)
        self.marker.unlink(missing_ok=True)
        if pre_loaded:
            self.marker.touch()
        return subprocess.run(
            ["/bin/bash", str(self.tree / "scripts" / "test-module.sh"), module_name],
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
        )

    def test_prefix_named_module_is_not_confused_with_a_longer_module(self):
        """A loaded hello_module2 must not read as hello_module state."""
        # A *different*, longer-named module is loaded; hello_module is not.
        self.base_line = "hello_module2 16384 0"
        result = self.run_suite("hello_module")
        self.assertEqual(result.returncode, 0, result.stdout[-600:])
        self.assertNotIn("already loaded", result.stdout, result.stdout[-600:])
        self.assertIn("Module is not loaded", result.stdout)
        self.assertIn("Module is loaded", result.stdout)
        self.assertIn("Module is unloaded", result.stdout)
        self.assertNotIn("failed to unload", result.stdout, result.stdout[-600:])

    def test_exact_name_matches_a_genuinely_loaded_module(self):
        """Control: the module's own lsmod line still drives the suite."""
        # other_mod is unrelated; hello_module is genuinely loaded at start.
        self.base_line = "other_mod 16384 0"
        result = self.run_suite("hello_module", pre_loaded=True)
        self.assertEqual(result.returncode, 0, result.stdout[-600:])
        self.assertIn("already loaded", result.stdout)
        self.assertIn("Module is unloaded", result.stdout)
        self.assertNotIn("failed to unload", result.stdout, result.stdout[-600:])


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
