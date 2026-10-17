#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py formula token handling.

Regression tests for the cell-reference tokenizer: the reference regex used
to match tokens that merely look like cell references, so shifting rows
silently corrupted
  - scientific-notation literals (B5*1E3 -> B7*1E5 after insert 2 at 1),
  - function names with trailing digits (LOG10 -> LOG12, ATAN2 -> ATAN4),
  - unquoted sheet names ending in digits (Q1!B5 -> Q3!B5).
Real references in the same formulas must keep shifting.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")

SHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:D6"/>
  <sheetData>
    <row r="2"><c r="D2"><f>B5*1E3</f></c></row>
    <row r="3"><c r="D3"><f>LOG10(B5)</f></c></row>
    <row r="4"><c r="D4"><f>ATAN2(B5,B6)</f></c></row>
    <row r="5"><c r="D5"><f>Q1!B5</f></c></row>
    <row r="6"><c r="D6"><f>$B$5+B$6</f></c></row>
    <row r="7"><c r="D7"><f>'Q1 Data'!B5</f></c></row>
  </sheetData>
</worksheet>
"""


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


class ShiftRowsFormulaTokenTests(unittest.TestCase):
    """Insert 2 rows at row 1, then assert what each formula turned into."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="shift_rows_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        with open(os.path.join(ws_dir, "sheet1.xml"), "w", encoding="utf-8") as fh:
            fh.write(SHEET_XML)
        proc = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "insert", "1", "2"],
            capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        tree = ET.parse(os.path.join(ws_dir, "sheet1.xml"))
        self.formulas = {}
        for row in tree.getroot().iter(_tag("row")):
            for cell in row:
                f_el = cell.find(_tag("f"))
                if f_el is not None and cell.get("r"):
                    self.formulas[cell.get("r")] = f_el.text

    def test_scientific_notation_literal_is_preserved(self):
        # D2 shifted to D4. B5 becomes B7, but the 1E3 literal must not become 1E5.
        self.assertEqual(self.formulas["D4"], "B7*1E3")

    def test_log10_function_name_is_preserved(self):
        self.assertEqual(self.formulas["D5"], "LOG10(B7)")

    def test_atan2_function_name_is_preserved(self):
        self.assertEqual(self.formulas["D6"], "ATAN2(B7,B8)")

    def test_unquoted_sheet_name_digits_are_preserved(self):
        # Q1 is a sheet name, not a column-row reference; only B5 shifts.
        self.assertEqual(self.formulas["D7"], "Q1!B7")

    def test_quoted_sheet_name_content_is_preserved(self):
        # The quoted-segment split must keep protecting quoted names.
        self.assertEqual(self.formulas["D9"], "'Q1 Data'!B7")

    def test_real_references_still_shift(self):
        self.assertEqual(self.formulas["D8"], "$B$7+B$8")


if __name__ == "__main__":
    unittest.main()
