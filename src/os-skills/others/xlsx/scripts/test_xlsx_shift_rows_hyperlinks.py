#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py hyperlink handling.

Regression tests for hyperlink coordinates: the script shifts rows, cells,
formulas, merged cells, conditional formats, data validations, tables,
charts and pivot sources — but used to leave <hyperlink ref> pointing at
the pre-shift coordinate, so after an insert the link decorated whatever
data slid into the old cell instead of the linked cell itself.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")

SHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <dimension ref="A1:D6"/>
  <sheetData>
    <row r="3"><c r="A3" t="s"><v>0</v></c></row>
    <row r="5"><c r="A5" t="s"><v>1</v></c></row>
  </sheetData>
  <hyperlinks>
    <hyperlink ref="A5" r:id="rId1"/>
    <hyperlink ref="C3" r:id="rId2"/>
  </hyperlinks>
</worksheet>
"""


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


class ShiftRowsHyperlinkTests(unittest.TestCase):
    """Insert 2 rows at row 1, then check where each hyperlink landed."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="shift_rows_hyperlink_")
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
        root = tree.getroot()
        self.hyperlinks = {
            hl.get(f"{{{NS_REL}}}id"): hl.get("ref")
            for hl in root.iter(_tag("hyperlink"))
        }
        self.cell_refs = [
            c.get("r")
            for row in root.iter(_tag("row"))
            for c in row
        ]

    def test_hyperlink_moves_with_its_row(self):
        # The linked text moved from A5 to A7; the link must follow it.
        self.assertEqual(self.hyperlinks.get("rId1"), "A7")

    def test_hyperlink_below_shift_point_moves_too(self):
        self.assertEqual(self.hyperlinks.get("rId2"), "C5")

    def test_cell_data_still_shifts(self):
        # Control: the cells themselves keep shifting as before.
        self.assertIn("A7", self.cell_refs)
        self.assertIn("A5", self.cell_refs)


if __name__ == "__main__":
    unittest.main()
