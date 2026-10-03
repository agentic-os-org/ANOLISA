#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for xlsx_shift_rows.py (stdlib unittest, no deps).

Run from this directory:
    python3 -m unittest test_xlsx_shift_rows -v
"""

import os
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import xlsx_shift_rows as xs  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

WORKSHEET_TEMPLATE = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{ns}">
  <dimension ref="A1:C10"/>
  <sheetData>
    <row r="7"><c r="A7"><v>7</v></c></row>
    <row r="8"><c r="A8"><v>8</v></c></row>
    <row r="9"><c r="A9"><v>9</v></c></row>
  </sheetData>
</worksheet>
""".format(ns=NS_MAIN)


class ShiftRowsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.sheet = os.path.join(self.tmp.name, "sheet1.xml")
        with open(self.sheet, "w", encoding="utf-8") as fh:
            fh.write(WORKSHEET_TEMPLATE)

    def _load_rows(self):
        root = ET.parse(self.sheet).getroot()
        sheet_data = root.find(f"{{{NS_MAIN}}}sheetData")
        rows = {}
        for row_el in sheet_data.findall(f"{{{NS_MAIN}}}row"):
            cell = row_el.find(f"{{{NS_MAIN}}}c")
            rows[row_el.get("r")] = cell.find(f"{{{NS_MAIN}}}v").text
        return root, rows

    def test_delete_row_drops_content_without_duplicate(self):
        """delete 8 1: row 8's content is gone, row 9 shifts up, no
        duplicate row addresses."""
        xs.process_worksheet(self.sheet, at=8, delta=-1)
        _, rows = self._load_rows()
        # Old row 8 ("8") is dropped, old row 9 ("9") becomes row 8.
        self.assertEqual(rows, {"7": "7", "8": "9"})

    def test_insert_row_shifts_down(self):
        """insert 7 1: rows >= 7 shift down by one."""
        xs.process_worksheet(self.sheet, at=7, delta=1)
        _, rows = self._load_rows()
        self.assertEqual(rows, {"8": "7", "9": "8", "10": "9"})


if __name__ == "__main__":
    unittest.main()
