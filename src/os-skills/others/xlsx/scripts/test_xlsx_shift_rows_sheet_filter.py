#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for worksheet-level filter and sort ranges in xlsx_shift_rows.py.

A worksheet can carry its own <autoFilter ref="..."> with a nested
<sortState>/<sortCondition>, or a standalone <sortState>. Those ranges select
rows, so inserting or deleting rows must move them exactly like the copies in
xl/tables/*.xml. Leaving them behind makes Excel filter/sort a range that no
longer covers the shifted data (the inserted row is silently excluded).
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
  <dimension ref="A1:D11"/>
  <sheetData>
    <row r="1"><c r="A1"><v>1</v></c></row>
    <row r="2"><c r="A2"><v>2</v></c></row>
    <row r="10"><c r="A10"><v>10</v></c></row>
    <row r="11"><c r="A11"><v>11</v></c></row>
  </sheetData>
  <autoFilter ref="A1:D11">
    <sortState ref="A2:D11"><sortCondition ref="B2:B11"/></sortState>
  </autoFilter>
  <sortState ref="A3:D11"><sortCondition ref="C3:C11" descending="1"/></sortState>
  <dataValidations count="1"><dataValidation sqref="B2:B11" type="list"/></dataValidations>
</worksheet>
"""


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


class WorksheetFilterRangeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.work_dir = self.tmp.name
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        self.sheet_path = os.path.join(ws_dir, "sheet1.xml")
        with open(self.sheet_path, "w", encoding="utf-8") as fh:
            fh.write(SHEET_XML)

    def run_shift(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, SCRIPT, self.work_dir, *args],
                              capture_output=True, text=True)

    def test_insert_expands_worksheet_filter_and_sort_ranges(self):
        result = self.run_shift("insert", "5", "2")
        self.assertEqual(result.returncode, 0, result.stderr)

        root = ET.parse(self.sheet_path).getroot()
        auto_filter = root.find(_tag("autoFilter"))
        self.assertIsNotNone(auto_filter)
        self.assertEqual(auto_filter.get("ref"), "A1:D13")

        sort_states = list(root.iter(_tag("sortState")))
        self.assertEqual([s.get("ref") for s in sort_states], ["A2:D13", "A3:D13"])
        conditions = [c.get("ref") for c in root.iter(_tag("sortCondition"))]
        self.assertEqual(conditions, ["B2:B13", "C3:C13"])

        # Sibling ranges must keep working.
        self.assertEqual(root.find(_tag("dimension")).get("ref"), "A1:D13")
        self.assertEqual(
            root.find(f"{_tag('dataValidations')}/{_tag('dataValidation')}").get("sqref"),
            "B2:B13",
        )

    def test_delete_contracts_worksheet_filter_and_sort_ranges(self):
        result = self.run_shift("delete", "5", "2")
        self.assertEqual(result.returncode, 0, result.stderr)

        root = ET.parse(self.sheet_path).getroot()
        self.assertEqual(root.find(_tag("autoFilter")).get("ref"), "A1:D9")
        self.assertEqual([s.get("ref") for s in root.iter(_tag("sortState"))],
                         ["A2:D9", "A3:D9"])

    def test_filter_above_edit_point_is_left_alone(self):
        with open(self.sheet_path, "w", encoding="utf-8") as fh:
            fh.write(SHEET_XML.replace('ref="A1:D11"', 'ref="A1:D4"')
                     .replace('ref="A2:D11"', 'ref="A2:D4"')
                     .replace('ref="B2:B11"', 'ref="B2:B4"')
                     .replace('ref="A3:D11"', 'ref="A3:D4"')
                     .replace('ref="C3:C11"', 'ref="C3:C4"'))
        result = self.run_shift("insert", "8", "1")
        self.assertEqual(result.returncode, 0, result.stderr)
        root = ET.parse(self.sheet_path).getroot()
        self.assertEqual(root.find(_tag("autoFilter")).get("ref"), "A1:D4")


if __name__ == "__main__":
    unittest.main()
