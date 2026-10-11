#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for libreoffice_recalc.py exit-code classification.

Regression tests for the exit-code routing in main(): exit 2 is reserved
for "Tier 2 unavailable" (LibreOffice missing). A recalculation failure
must exit 1 whatever its message says — in particular when LibreOffice
itself fails and its raw stderr happens to contain the words "not found",
which the old substring-based routing misclassified as an unavailable
Tier 2 and let pipelines skip validation on a failing file.
"""

import io
import os
import stat
import sys
import tempfile
import unittest
import zipfile
from contextlib import redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import libreoffice_recalc as lr  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

# A soffice that answers --version, then fails with stderr containing
# "not found" — plausible wording for a source/filter/profile error.
STUB_FAIL_NOTFOUND_POSIX = """#!/bin/sh
case "$*" in
  *--version*) echo "LibreOffice 24.8.4.2 (stub)"; exit 0;;
  *) echo "Error: source file not found" >&2; exit 1;;
esac
"""

STUB_FAIL_NOTFOUND_WINDOWS = (
    "@echo off\r\n"
    "if \"%1\"==\"--version\" (\r\n"
    "  echo LibreOffice 24.8.4.2 stub\r\n"
    "  exit /b 0\r\n"
    ")\r\n"
    "echo Error: source file not found 1>&2\r\n"
    "exit /b 1\r\n"
)


def write_fail_stub(directory: str) -> str:
    if os.name == "nt":
        path = os.path.join(directory, "soffice_fail.bat")
        body = STUB_FAIL_NOTFOUND_WINDOWS
    else:
        path = os.path.join(directory, "soffice_fail")
        body = STUB_FAIL_NOTFOUND_POSIX
    with open(path, "w", newline="") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def make_tiny_xlsx(path: str) -> str:
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


class TestRecalcExitCodeRouting(unittest.TestCase):
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

    def test_failing_soffice_with_notfound_stderr_is_exit_1(self):
        """A recalculation failure whose stderr contains "not found" must exit 1.

        The old routing matched the substring "not found" in the failure
        message and exited 2 ("Tier 2 unavailable"), so a file LibreOffice
        actually failed on was reported as an environment skip and pipelines
        silently skipped dynamic validation.
        """
        stub = write_fail_stub(self.root)
        lr.find_soffice = lambda: stub
        sys.argv = ["libreoffice_recalc.py", self.input_xlsx, self.output_xlsx]
        code, out = self.run_main()
        self.assertEqual(code, 1, "a real recalculation failure must exit 1")
        self.assertNotIn("Tier 2 unavailable", out)
        self.assertFalse(os.path.exists(self.output_xlsx))

    def test_missing_soffice_is_exit_2(self):
        """Only a missing LibreOffice keeps the exit-2 Tier 2 skip."""
        lr.find_soffice = lambda: None
        sys.argv = ["libreoffice_recalc.py", self.input_xlsx, self.output_xlsx]
        code, out = self.run_main()
        self.assertEqual(code, 2)
        self.assertIn("Tier 2 unavailable", out)


if __name__ == "__main__":
    unittest.main()
