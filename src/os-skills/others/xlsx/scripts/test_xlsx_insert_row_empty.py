#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_insert_row.py valueless inserts.

Inserting a row without any --text/--values/--formula cells is a valid
operation (shift the rows, add an empty row), but the dimension update
used to call max() over the empty column set and crashed with
"ValueError: max() arg is an empty sequence".
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_insert_row.py")
UNPACK = os.path.join(SCRIPTS_DIR, "xlsx_unpack.py")

NS_SS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"


def make_numeric_workbook(path):
    import openpyxl

    wb = openpyxl.Workbook()
    ws = wb.active
    for i in range(3):
        ws.cell(row=i + 1, column=1, value=(i + 1) * 10)
    wb.save(path)


class XlsxInsertRowEmptyTest(unittest.TestCase):

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

    def tearDown(self):
        self.tmp.cleanup()

    def test_insert_without_cells_adds_an_empty_row(self):
        before = subprocess.run(
            [sys.executable, "-c",
             "import xml.etree.ElementTree as ET;"
             "t=ET.parse('%s/xl/worksheets/sheet1.xml');"
             "print(sorted(int(r.get('r')) for r in t.getroot().iter('{http://schemas.openxmlformats.org/spreadsheetml/2006/main}row')))"
             % self.work],
            capture_output=True, text=True,
        )
        self.assertEqual(before.stdout.strip(), "[1, 2, 3]")

        result = subprocess.run(
            [sys.executable, SCRIPT, self.work, "--at", "2"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("Traceback", result.stderr)

        sheet = ET.parse(os.path.join(self.work, "xl", "worksheets", "sheet1.xml"))
        rows = sorted(int(r.get("r")) for r in sheet.getroot().iter(f"{NS_SS}row"))
        self.assertEqual(rows, [1, 2, 3, 4])
        empty_row = next(
            r for r in sheet.getroot().iter(f"{NS_SS}row") if r.get("r") == "2"
        )
        self.assertEqual(len(list(empty_row)), 0, "inserted row should be empty")

    def test_insert_without_cells_keeps_the_dimension_columns(self):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work, "--at", "2"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        sheet = ET.parse(os.path.join(self.work, "xl", "worksheets", "sheet1.xml"))
        dimension = sheet.getroot().find(f"{NS_SS}dimension")
        self.assertEqual(dimension.get("ref"), "A1:A4")


if __name__ == "__main__":
    unittest.main()
