#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for libreoffice_recalc.py exit-code classification.

A `soffice` that exits 0 without converting anything must not be reported as a
successful recalculation, and the failure must not be classified as the
"Tier 2 unavailable" skip. The stub below never touches a real LibreOffice.
"""

import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"
SCRIPT = SCRIPTS_DIR / "libreoffice_recalc.py"

CONVERTING_STUB = """#!/usr/bin/env python3
import os
import sys

if "--version" in sys.argv:
    print("LibreOffice 24.8.4.2 stub")
    sys.exit(0)

outdir = sys.argv[sys.argv.index("--outdir") + 1]
source = sys.argv[-1]
stem = os.path.splitext(os.path.basename(source))[0]
with open(os.path.join(outdir, stem + ".xlsx"), "wb") as handle:
    handle.write(b"recalculated")
sys.exit(0)
"""

NOOP_STUB = """#!/usr/bin/env python3
import sys

sys.dont_write_bytecode = True

if "--version" in sys.argv:
    print("LibreOffice 24.8.4.2 stub")
sys.exit(0)
"""


class TestRecalculateOutcome(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.bin_dir = Path(self.tmpdir) / "bin"
        self.bin_dir.mkdir()
        self.input_path = Path(self.tmpdir) / "sample.xlsx"
        self.input_path.write_bytes(b"original workbook")
        self.output_path = Path(self.tmpdir) / "out.xlsx"

    def _install_stub(self, body):
        stub = self.bin_dir / "soffice"
        stub.write_text(body, encoding="utf-8")
        stub.chmod(stub.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
        return stub

    def _run(self):
        env = dict(os.environ)
        env["PATH"] = str(self.bin_dir) + os.pathsep + env.get("PATH", "")
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(self.input_path), str(self.output_path)],
            capture_output=True,
            text=True,
            check=False,
            env=env,
        )

    def test_noop_soffice_fails_instead_of_shipping_the_input(self):
        self._install_stub(NOOP_STUB)

        result = self._run()

        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("without converting", result.stdout)
        self.assertNotIn("SKIP", result.stdout)
        self.assertFalse(self.output_path.exists())

    def test_converting_soffice_writes_the_recalculated_workbook(self):
        self._install_stub(CONVERTING_STUB)

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Recalculation complete", result.stdout)
        self.assertEqual(self.output_path.read_bytes(), b"recalculated")


if __name__ == "__main__":
    unittest.main()
