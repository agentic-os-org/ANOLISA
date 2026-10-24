#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_add_column.py idempotency and argument validation.

Regression tests for two defect classes:
1. re-running the script appended duplicate <c> elements at the same refs —
   invalid OOXML that Excel's repair dialog "fixes" by dropping cells;
2. bad arguments (--col 7, --col G2, --formula-rows 2-9 or 9:2, --total-row 0)
   produced corrupt refs, raw tracebacks, or a fake success message with the
   whole column missing.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_add_column.py")

NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def T(local):
    return f"{{{NS_SS}}}{local}"


WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_SS}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""

WORKBOOK_RELS_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="/xl/worksheets/sheet1.xml"/>
</Relationships>
"""

STYLES_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_SS}">
  <fonts count="1"><font><sz val="11"/></font></fonts>
  <fills count="1"><fill><patternFill patternType="none"/></fill></fills>
  <borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="2">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
    <xf numFmtId="0" fontId="0" fillId="1" borderId="0" xfId="0"/>
  </cellXfs>
</styleSheet>
"""

SHARED_STRINGS_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="{NS_SS}" count="3" uniqueCount="3">
  <si><t>Region</t></si>
  <si><t>North</t></si>
  <si><t>South</t></si>
</sst>
"""

# Rows 1-3, columns A and F (F carries style 1 so style copying is observable).
SHEET1_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SS}">
  <dimension ref="A1:F3"/>
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="F1" t="s" s="1"><v>0</v></c>
    </row>
    <row r="2">
      <c r="A2" t="s"><v>1</v></c>
      <c r="F2" s="1"><v>10</v></c>
    </row>
    <row r="3">
      <c r="A3" t="s"><v>2</v></c>
      <c r="F3" s="1"><v>20</v></c>
    </row>
  </sheetData>
</worksheet>
"""


class AddColumnTestCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.work = os.path.join(self._tmp.name, "work")
        for path, content in [
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS_XML),
            ("xl/styles.xml", STYLES_XML),
            ("xl/sharedStrings.xml", SHARED_STRINGS_XML),
            ("xl/worksheets/sheet1.xml", SHEET1_XML),
        ]:
            full = os.path.join(self.work, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w", encoding="utf-8") as f:
                f.write(content)

    def tearDown(self):
        self._tmp.cleanup()

    def run_script(self, *args):
        return subprocess.run(
            [sys.executable, SCRIPT, self.work, *args],
            capture_output=True,
            text=True,
        )

    def sheet_root(self):
        return ET.parse(os.path.join(self.work, "xl/worksheets/sheet1.xml")).getroot()

    def cells_in_row(self, row):
        for row_el in self.sheet_root().iter(T("row")):
            if row_el.get("r") == str(row):
                return list(row_el)
        return []

    def cell_refs_in_row(self, row):
        return [c.get("r") for c in self.cells_in_row(row)]


class Idempotency(AddColumnTestCase):
    ARGS = ("--col", "G", "--header", "Pct",
            "--formula", "=F{row}", "--formula-rows", "2:3")

    def test_rerun_does_not_duplicate_cells(self):
        first = self.run_script(*self.ARGS)
        self.assertEqual(first.returncode, 0, first.stderr)
        second = self.run_script(*self.ARGS)
        self.assertEqual(second.returncode, 0, second.stderr)
        for row in (1, 2, 3):
            refs = self.cell_refs_in_row(row)
            self.assertEqual(refs.count(f"G{row}"), 1,
                             f"duplicate G{row} after re-run: {refs}")
        self.assertIn("updated existing", second.stdout)

    def test_upsert_replaces_value_and_formula(self):
        self.run_script(*self.ARGS)
        second = self.run_script(
            "--col", "G", "--formula", "=F{row}*2", "--formula-rows", "2:2")
        self.assertEqual(second.returncode, 0, second.stderr)
        g2 = [c for c in self.cells_in_row(2) if c.get("r") == "G2"][0]
        f_el = g2.find(T("f"))
        self.assertIsNotNone(f_el, "G2 must carry a formula after update")
        self.assertEqual(f_el.text, "F2*2")
        self.assertIsNone(g2.find(T("v")), "stale <v> must not survive the update")

    def test_rerun_shared_strings_not_duplicated(self):
        self.run_script(*self.ARGS)
        self.run_script(*self.ARGS)
        ss = ET.parse(os.path.join(self.work, "xl/sharedStrings.xml")).getroot()
        headers = [si for si in ss.findall(T("si"))
                   if si.find(T("t")) is not None and si.find(T("t")).text == "Pct"]
        self.assertEqual(len(headers), 1)

    def test_valid_first_run_unchanged(self):
        result = self.run_script(*self.ARGS)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.cell_refs_in_row(1), ["A1", "F1", "G1"])
        self.assertEqual(self.cell_refs_in_row(2), ["A2", "F2", "G2"])
        dim = next(self.sheet_root().iter(T("dimension")))
        self.assertEqual(dim.get("ref"), "A1:G3")
        g1 = [c for c in self.cells_in_row(1) if c.get("r") == "G1"][0]
        self.assertEqual(g1.get("s"), "1", "header style copied from column F")

    def test_midrow_column_insert_keeps_order(self):
        result = self.run_script("--col", "C", "--formula", "=A{row}",
                                 "--formula-rows", "2:2")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.cell_refs_in_row(2), ["A2", "C2", "F2"],
                         "cells must stay in ascending column order")


class Validation(AddColumnTestCase):
    def assert_usage_error(self, result, needle):
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(needle, result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_col_rejects_digits_and_cell_refs(self):
        for bad in ("7", "G2", "A_B", "FFFF"):
            with self.subTest(col=bad):
                before = open(os.path.join(
                    self.work, "xl/worksheets/sheet1.xml")).read()
                result = self.run_script("--col", bad, "--header", "X")
                self.assert_usage_error(result, "--col")
                after = open(os.path.join(
                    self.work, "xl/worksheets/sheet1.xml")).read()
                self.assertEqual(before, after, "sheet must be untouched")

    def test_formula_rows_rejects_hyphen_and_reversed(self):
        for bad in ("2-9", "9:2", ":9", "2:", "0:3"):
            with self.subTest(rows=bad):
                result = self.run_script("--col", "G", "--formula", "=F{row}",
                                         "--formula-rows", bad)
                self.assert_usage_error(result, "--formula-rows")

    def test_total_row_must_be_positive_and_not_header_row(self):
        result = self.run_script("--col", "G", "--total-row", "0",
                                 "--total-formula", "=SUM(G2:G3)")
        self.assert_usage_error(result, "--total-row")
        result = self.run_script("--col", "G", "--header", "H",
                                 "--total-row", "1",
                                 "--total-formula", "=SUM(G2:G3)")
        self.assert_usage_error(result, "--total-row")

    def test_corrupt_worksheet_xml_fails_cleanly(self):
        sheet = os.path.join(self.work, "xl/worksheets/sheet1.xml")
        with open(sheet, "w", encoding="utf-8") as f:
            f.write("<not-xml")
        result = self.run_script("--col", "G", "--header", "X")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("not well-formed", result.stderr)
        self.assertIn(sheet, result.stderr)
        self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
