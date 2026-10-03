#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for libreoffice_recalc.py output verification.

Regression tests for the vacuous output-existence check: the input copy
and the expected LibreOffice output must be distinct paths, and a
soffice that exits 0 without converting anything must be reported as a
failed recalculation (exit 1), never as success. Tests stub the soffice
binary — a real LibreOffice is never invoked.
"""

import io
import os
import stat
import subprocess
import sys
import tempfile
import unittest
import zipfile
from contextlib import redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import libreoffice_recalc as lr  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

# Stub soffice that answers --version, then exits 0 writing no output.
STUB_NOOP = """#!/bin/sh
case "$*" in
  *--version*) echo "LibreOffice 24.8.4.2 (stub)"; exit 0;;
  *) echo "convert: no output produced"; exit 0;;
esac
"""

# Stub soffice that answers --version and mimics --convert-to: it copies the
# input file into the directory given after --outdir.
STUB_CONVERT = """#!/bin/sh
case "$*" in
  *--version*) echo "LibreOffice 24.8.4.2 (stub)"; exit 0;;
esac
outdir=""
last=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--outdir" ]; then outdir="$arg"; fi
  case "$arg" in -*) ;; *) last="$arg";; esac
  prev="$arg"
done
name=$(basename "$last")
cp "$last" "$outdir/$name"
echo "convert $last -> $outdir/$name using filter Calc MS Excel 2007 XML"
exit 0
"""


def write_stub(path: str, body: str) -> str:
    with open(path, "w") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def make_tiny_xlsx(path: str) -> str:
    """Write a minimal valid-enough xlsx archive for byte-copy stubs."""
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml",
                   '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                   '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                   '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
                   '<Default Extension="xml" ContentType="application/xml"/>'
                   '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
                   '</Types>')
        z.writestr("_rels/.rels",
                   '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                   '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                   '<Relationship Id="rId1" '
                   'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" '
                   'Target="xl/workbook.xml"/></Relationships>')
        z.writestr("xl/workbook.xml",
                   f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                   f'<workbook xmlns="{NS_MAIN}"><sheets/></workbook>')
    return path


class TestRecalcOutputCheck(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = self._tmp.name
        self.input_xlsx = make_tiny_xlsx(os.path.join(self.root, "model.xlsx"))
        self.output_xlsx = os.path.join(self.root, "model_recalc.xlsx")
        self._orig_argv = sys.argv
        self._orig_find = lr.find_soffice

    def tearDown(self):
        sys.argv = self._orig_argv
        lr.find_soffice = self._orig_find
        self._tmp.cleanup()

    def run_main(self) -> tuple[int, str]:
        buf = io.StringIO()
        code = 0
        with redirect_stdout(buf):
            try:
                lr.main()
            except SystemExit as e:
                code = e.code
        return code, buf.getvalue()

    def test_noop_soffice_is_a_failed_recalculation(self):
        """Exit-0 soffice that writes nothing must fail with exit 1 (issue #3647)."""
        stub = write_stub(os.path.join(self.root, "soffice_noop"), STUB_NOOP)
        lr.find_soffice = lambda: stub
        sys.argv = ["libreoffice_recalc.py", self.input_xlsx, self.output_xlsx]
        code, out = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("recalculation failed", out)
        # Must be the exit-1 failure path, not the exit-2 Tier 2 skip wording
        self.assertNotIn("not found", out.lower())
        self.assertFalse(os.path.exists(self.output_xlsx))

    def test_converting_soffice_succeeds(self):
        """A soffice that writes the converted file is a success (exit 0)."""
        stub = write_stub(os.path.join(self.root, "soffice_convert"), STUB_CONVERT)
        lr.find_soffice = lambda: stub
        sys.argv = ["libreoffice_recalc.py", self.input_xlsx, self.output_xlsx]
        code, out = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("Recalculation complete", out)
        self.assertTrue(os.path.isfile(self.output_xlsx))
        with open(self.input_xlsx, "rb") as f:
            src = f.read()
        with open(self.output_xlsx, "rb") as f:
            self.assertEqual(f.read(), src)

    def test_missing_soffice_still_skips_with_exit_2(self):
        """The Tier 2-unavailable routing (exit 2) is unchanged."""
        lr.find_soffice = lambda: None
        sys.argv = ["libreoffice_recalc.py", self.input_xlsx, self.output_xlsx]
        code, out = self.run_main()
        self.assertEqual(code, 2)
        self.assertIn("Tier 2 unavailable", out)


if __name__ == "__main__":
    unittest.main()
