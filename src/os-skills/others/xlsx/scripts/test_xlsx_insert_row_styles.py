#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_insert_row.py style sourcing.

Regression test for the reference-row timing: --copy-style-from styles
used to be read from the post-shift tree, so when the reference row was
at or below the insertion point the new row silently received the styles
of the row that had been shifted INTO that position — with --at 3
--copy-style-from 4, the new row got original row 3's styles, not row
4's.
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


def make_fixture(path):
    """Four rows, each with a distinct cell style, so a copied style is
    attributable to exactly one original row."""
    import openpyxl
    from openpyxl.styles import Font

    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Data"
    colors = ["FF0000", "00AA00", "0000FF", "AA00AA"]
    for i, color in enumerate(colors):
        for col in (1, 2):
            cell = ws.cell(row=i + 1, column=col, value=f"r{i + 1}c{col}")
            cell.font = Font(color=color)
    wb.save(path)


def read_styles(work_dir):
    """Return {row_number: {cell_ref: style_index}} of the sheet."""
    sheet = ET.parse(os.path.join(work_dir, "xl", "worksheets", "sheet1.xml"))
    styles = {}
    for row in sheet.getroot().iter(f"{NS_SS}row"):
        styles[int(row.get("r"))] = {c.get("r"): c.get("s") for c in row}
    return styles


class XlsxInsertRowStyleTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.work = os.path.join(self.tmp.name, "work")
        fixture = os.path.join(self.tmp.name, "fixture.xlsx")
        make_fixture(fixture)
        env = dict(os.environ)
        env["PYTHONPATH"] = SCRIPTS_DIR
        unpacked = subprocess.run(
            [sys.executable, UNPACK, fixture, self.work],
            capture_output=True, text=True, env=env,
        )
        self.assertEqual(unpacked.returncode, 0, unpacked.stderr)
        self.original = read_styles(self.work)

    def tearDown(self):
        self.tmp.cleanup()

    def insert(self, *flags):
        return subprocess.run(
            [sys.executable, SCRIPT, self.work, *flags],
            capture_output=True, text=True,
        )

    def test_reference_row_below_insertion_point_keeps_its_own_styles(self):
        # Reference row 4, insert at 3: after the shift, position 4 holds
        # original row 3, so reading styles post-shift would copy row 3's
        # styles. The caller asked for row 4's.
        # Numeric cells only: the fixture's openpyxl output carries inline
        # strings without a sharedStrings part, and creating it is a separate
        # concern from the style timing under test.
        result = self.insert(
            "--at", "3", "--values", "B=99", "--copy-style-from", "4"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        after = read_styles(self.work)
        self.assertEqual(after[3]["B3"], self.original[4]["B4"])

    def test_reference_row_above_insertion_point_is_unaffected(self):
        # Control case: the reference row above the insertion point does not
        # move, and its styles land on the new row either way.
        result = self.insert(
            "--at", "3", "--values", "B=99", "--copy-style-from", "1"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        after = read_styles(self.work)
        self.assertEqual(after[3]["B3"], self.original[1]["B1"])


if __name__ == "__main__":
    unittest.main()
