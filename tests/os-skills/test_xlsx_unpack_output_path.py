#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for xlsx_unpack.py output path handling.

xlsx_unpack.py guarded the output directory with `os.path.exists` +
`shutil.rmtree(output_dir)`. When the given output path is an existing
regular file (or a symlink), rmtree raises NotADirectoryError / OSError
and the user gets a bare traceback instead of the script's diagnostic
style. The tool must refuse such paths with a clean error, and keep
replacing an existing real directory.

Baseline on unchanged main: 2 failures / 1 passing control.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
    / "xlsx_unpack.py"
)


class XlsxUnpackOutputPathTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.xlsx = self.root / "in.xlsx"
        with zipfile.ZipFile(self.xlsx, "w") as z:
            z.writestr("xl/workbook.xml", "<workbook/>")

    def run_unpack(self, output):
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(self.xlsx), str(output)],
            capture_output=True,
            text=True,
            timeout=60,
        )

    def assert_clean_refusal(self, result, output):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr, result.stderr)
        self.assertIn("ERROR", result.stderr)
        self.assertIn(str(output), result.stderr)

    def test_existing_file_output_dir_is_refused_cleanly(self):
        output = self.root / "outfile"
        output.write_text("i am a file\n", encoding="utf-8")
        result = self.run_unpack(output)
        self.assert_clean_refusal(result, output)
        # The pre-existing file is left untouched.
        self.assertEqual(output.read_text(encoding="utf-8"), "i am a file\n")

    def test_symlink_output_dir_is_refused_cleanly(self):
        if os.name != "posix":
            raise unittest.SkipTest("symlink semantics need a posix host")
        real = self.root / "real-dir"
        real.mkdir()
        link = self.root / "link-dir"
        link.symlink_to(real, target_is_directory=True)
        result = self.run_unpack(link)
        self.assert_clean_refusal(result, link)
        # The link and its target survive untouched.
        self.assertTrue(link.is_symlink())
        self.assertTrue(real.is_dir())

    def test_existing_directory_output_is_replaced(self):
        """Control: a real directory is wiped and rebuilt as before."""
        output = self.root / "out"
        output.mkdir()
        (output / "stale.txt").write_text("old\n", encoding="utf-8")
        result = self.run_unpack(output)
        self.assertEqual(result.returncode, 0, result.stderr[-400:])
        self.assertIn("Unpacked", result.stdout)
        self.assertFalse((output / "stale.txt").exists())
        self.assertTrue((output / "xl" / "workbook.xml").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2, exit=False)
