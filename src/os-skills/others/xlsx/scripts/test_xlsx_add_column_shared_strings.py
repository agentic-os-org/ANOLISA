#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_add_column.py first-text-cell handling.

A workbook without shared strings is valid OOXML — openpyxl writes
inline strings, so its files carry no xl/sharedStrings.xml part. Adding
the first text cell (the --header) used to crash with a raw
FileNotFoundError instead of creating the part together with its package
registrations.
"""

import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_add_column.py")
UNPACK = os.path.join(SCRIPTS_DIR, "xlsx_unpack.py")
PACK = os.path.join(SCRIPTS_DIR, "xlsx_pack.py")


def make_numeric_workbook(path):
    """Numbers only: no sharedStrings part exists."""
    import openpyxl

    wb = openpyxl.Workbook()
    ws = wb.active
    for i in range(3):
        ws.cell(row=i + 1, column=1, value=(i + 1) * 10)
    wb.save(path)


class XlsxAddColumnSharedStringsTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.work = os.path.join(self.tmp.name, "work")
        fixture = os.path.join(self.tmp.name, "numeric.xlsx")
        make_numeric_workbook(fixture)
        unpacked = subprocess.run(
            [sys.executable, UNPACK, fixture, self.work],
            capture_output=True, text=True,
        )
        self.assertEqual(unpacked.returncode, 0, unpacked.stderr)
        self.assertFalse(
            os.path.exists(os.path.join(self.work, "xl", "sharedStrings.xml"))
        )

    def tearDown(self):
        self.tmp.cleanup()

    def test_header_creates_the_shared_strings_part_and_registers_it(self):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work, "--col", "B", "--header", "Total"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

        ss_path = os.path.join(self.work, "xl", "sharedStrings.xml")
        self.assertTrue(os.path.exists(ss_path), "sharedStrings.xml not created")
        with open(ss_path, encoding="utf-8") as handle:
            self.assertIn("Total", handle.read())
        with open(
            os.path.join(self.work, "[Content_Types].xml"), encoding="utf-8"
        ) as handle:
            self.assertIn("/xl/sharedStrings.xml", handle.read())
        with open(
            os.path.join(self.work, "xl", "_rels", "workbook.xml.rels"),
            encoding="utf-8",
        ) as handle:
            self.assertIn("sharedStrings", handle.read())

    def test_repacked_workbook_opens_and_reads_the_header(self):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work, "--col", "B", "--header", "Total"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        packed = os.path.join(self.tmp.name, "packed.xlsx")
        repacked = subprocess.run(
            [sys.executable, PACK, self.work, packed],
            capture_output=True, text=True,
        )
        self.assertEqual(repacked.returncode, 0, repacked.stderr)

        import openpyxl

        wb = openpyxl.load_workbook(packed)
        self.assertEqual(wb.active["B1"].value, "Total")
        self.assertEqual(wb.active["A3"].value, 30)


if __name__ == "__main__":
    unittest.main()
