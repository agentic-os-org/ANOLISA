#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for invalid `s` (style) attribute values in the xlsx edit scripts.

A cell may carry an empty or non-numeric `s` attribute (hand-edited XML,
third-party writers). Both editors used `int(cell.get("s", "0"))` and died
with a ValueError traceback: xlsx_insert_row.py after it had already shifted
the workbook (leaving a half-edited work dir), xlsx_add_column.py before it
could add anything. style_audit.py already treats such values as "no style";
the editors must do the same instead of crashing.
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

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

RELS_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="/xl/worksheets/sheet1.xml"/>
</Relationships>
"""

# A2/A3 carry invalid style attributes; B2 is fine and must still be copied.
SHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:C3"/>
  <sheetData>
    <row r="1"><c r="A1" t="s" s="1"><v>0</v></c><c r="B1" s="1"><v>1</v></c></row>
    <row r="2"><c r="A2" t="s" s=""><v>1</v></c><c r="B2" s="1"><v>10</v></c></row>
    <row r="3"><c r="A3" t="s" s="abc"><v>2</v></c><c r="B3" s="2"><v>20</v></c></row>
  </sheetData>
</worksheet>
"""

SST_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{NS_MAIN}" count="3" uniqueCount="3">
  <si><t>Item</t></si><si><t>Widget</t></si><si><t>Gadget</t></si>
</sst>
"""


class InvalidStyleAttributeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work_dir = self.tmp.name
        os.makedirs(os.path.join(self.work_dir, "xl", "worksheets"))
        os.makedirs(os.path.join(self.work_dir, "xl", "_rels"))
        self._write("xl/workbook.xml", WORKBOOK_XML)
        self._write("xl/_rels/workbook.xml.rels", RELS_XML)
        self._write("xl/sharedStrings.xml", SST_XML)
        self._write("xl/worksheets/sheet1.xml", SHEET_XML)
        self.sheet_path = os.path.join(self.work_dir, "xl", "worksheets", "sheet1.xml")

    def _write(self, rel: str, content: str) -> None:
        with open(os.path.join(self.work_dir, rel), "w", encoding="utf-8") as fh:
            fh.write(content)

    def test_insert_row_with_invalid_style_attributes(self):
        result = subprocess.run(
            [sys.executable, os.path.join(SCRIPTS_DIR, "xlsx_insert_row.py"),
             self.work_dir, "--at", "4", "--text", "A=New", "--values", "B=30",
             "--copy-style-from", "2"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("Traceback", result.stderr)

        root = ET.parse(self.sheet_path).getroot()
        cells = {c.get("r"): c for c in root.iter(f"{{{NS_MAIN}}}c")}
        # The row was inserted and the usable reference style was copied.
        self.assertIn("A4", cells)
        self.assertIn("B4", cells)
        self.assertEqual(cells["B4"].get("s"), "1")

    def test_add_column_with_invalid_style_attribute_in_previous_column(self):
        # C2 is missing entirely, so the style of the previous column (B) is
        # used: B3 has s="2", B2 has a valid style. Point the formula range at
        # row 2 where an invalid value sits on the *previous* column.
        self._write("xl/worksheets/sheet1.xml", SHEET_XML.replace(
            '<c r="B2" s="1"><v>10</v></c>', '<c r="B2" s=""><v>10</v></c>'))
        result = subprocess.run(
            [sys.executable, os.path.join(SCRIPTS_DIR, "xlsx_add_column.py"),
             self.work_dir, "--col", "C", "--header", "Extra",
             "--formula", "=B{row}*2", "--formula-rows", "2:2"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("Traceback", result.stderr)

        root = ET.parse(self.sheet_path).getroot()
        cells = {c.get("r"): c for c in root.iter(f"{{{NS_MAIN}}}c")}
        self.assertEqual(cells["C2"].find(f"{{{NS_MAIN}}}f").text, "B2*2")


if __name__ == "__main__":
    unittest.main()
