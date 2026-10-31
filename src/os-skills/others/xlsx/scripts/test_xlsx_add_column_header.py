#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_add_column.py header placement.

A sheet whose first <row> element starts below row 1 (data begins at
row 2+) used to have its --header silently dropped: the header path
required an existing row-1 element, unlike the formula path, which
creates missing rows. The header string still landed in sharedStrings
and the run reported success with one cell fewer than requested.

The fixture is a hand-built minimal package (not openpyxl output, whose
inline strings would require the shared-strings part bootstrap tracked
separately).
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_add_column.py")
PACK = os.path.join(SCRIPTS_DIR, "xlsx_pack.py")

NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

WORKBOOK = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_SS}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
'''

WORKBOOK_RELS = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>
'''

# Data starts at row 2: sheetData has no row-1 element.
SHEET = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SS}">
  <dimension ref="A2:A3"/>
  <sheetData>
    <row r="2"><c r="A2" t="s"><v>0</v></c></row>
    <row r="3"><c r="A3" t="s"><v>1</v></c></row>
  </sheetData>
</worksheet>
'''

SHARED_STRINGS = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{NS_SS}" count="2" uniqueCount="2">
  <si><t>alpha</t></si>
  <si><t>beta</t></si>
</sst>
'''

CONTENT_TYPES = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>
'''


class XlsxAddColumnHeaderTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.work = os.path.join(self.tmp.name, "work")
        os.makedirs(os.path.join(self.work, "xl", "worksheets"))
        os.makedirs(os.path.join(self.work, "xl", "_rels"))
        os.makedirs(os.path.join(self.work, "_rels"))
        with open(os.path.join(self.work, "xl", "workbook.xml"), "w") as fh:
            fh.write(WORKBOOK)
        with open(
            os.path.join(self.work, "xl", "_rels", "workbook.xml.rels"), "w"
        ) as fh:
            fh.write(WORKBOOK_RELS)
        with open(
            os.path.join(self.work, "xl", "worksheets", "sheet1.xml"), "w"
        ) as fh:
            fh.write(SHEET)
        with open(os.path.join(self.work, "xl", "sharedStrings.xml"), "w") as fh:
            fh.write(SHARED_STRINGS)
        with open(os.path.join(self.work, "[Content_Types].xml"), "w") as fh:
            fh.write(CONTENT_TYPES)

    def tearDown(self):
        self.tmp.cleanup()

    def sheet_data(self):
        sheet = ET.parse(
            os.path.join(self.work, "xl", "worksheets", "sheet1.xml")
        )
        return sheet.getroot().find(f"{{{NS_SS}}}sheetData")

    def test_header_is_added_when_row_one_does_not_exist(self):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work, "--col", "B", "--header", "Title"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

        sheet_data = self.sheet_data()
        rows = [int(r.get("r")) for r in sheet_data]
        self.assertIn(1, rows, "row 1 was not created for the header")
        # Row 1 must come first in document order.
        self.assertEqual(rows[0], 1)
        row_one = next(r for r in sheet_data if r.get("r") == "1")
        self.assertEqual(
            [c.get("r") for c in row_one], ["B1"], "header cell missing in row 1"
        )

    def test_existing_first_row_header_is_unchanged(self):
        # Control: a sheet whose data starts at row 1 keeps working.
        with open(
            os.path.join(self.work, "xl", "worksheets", "sheet1.xml"), "w"
        ) as fh:
            fh.write(SHEET.replace('<row r="2">', '<row r="1">').replace(
                '<row r="3">', '<row r="2">').replace('r="A2"', 'r="A1"').replace(
                'r="A3"', 'r="A2"').replace('ref="A2:A3"', 'ref="A1:A2"'))
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work, "--col", "B", "--header", "Title"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        sheet_data = self.sheet_data()
        row_one = next(r for r in sheet_data if r.get("r") == "1")
        self.assertEqual([c.get("r") for c in row_one], ["A1", "B1"])


if __name__ == "__main__":
    unittest.main()
