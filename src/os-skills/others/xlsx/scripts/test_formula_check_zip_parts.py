#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py on degenerate zip packages.

A valid ZIP that is not an xlsx (no xl/workbook.xml), or a workbook
without its .rels part, used to escape check() as a raw KeyError
instead of the documented file_error result with exit code 1.
"""

import os
import sys
import tempfile
import unittest
import zipfile

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

from formula_check import check  # noqa: E402

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def _workbook_xml() -> str:
    return (
        '<?xml version="1.0"?>'
        f'<workbook xmlns="{NS}" xmlns:r="{NS_REL}">'
        '<sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>'
    )


class TestDegenerateZipPackages(unittest.TestCase):
    def test_zip_without_workbook_reports_file_error(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "plain.zip")
            with zipfile.ZipFile(path, "w") as z:
                z.writestr("hello.txt", "not an xlsx")
            results = check(path)
            self.assertEqual(results["error_count"], 1)
            self.assertEqual(results["errors"][0]["type"], "file_error")
            self.assertIn("workbook", results["errors"][0]["message"])

    def test_workbook_without_rels_reports_file_error(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "no_rels.xlsx")
            with zipfile.ZipFile(path, "w") as z:
                z.writestr("xl/workbook.xml", _workbook_xml())
            results = check(path)
            self.assertEqual(results["error_count"], 1)
            self.assertEqual(results["errors"][0]["type"], "file_error")
            self.assertIn("workbook.xml.rels", results["errors"][0]["message"])

    def test_bad_zip_still_reports_file_error(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "garbage.xlsx")
            with open(path, "wb") as f:
                f.write(b"not a zip at all")
            results = check(path)
            self.assertEqual(results["error_count"], 1)
            self.assertEqual(results["errors"][0]["type"], "file_error")


if __name__ == "__main__":
    unittest.main()
