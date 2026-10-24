#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""
test_xlsx_add_column.py — Regression tests for xlsx_add_column.py.

Covers issue #3745: rows created for formula ranges / total rows were
appended at the tail of <sheetData>, breaking the ascending row order
required by SpreadsheetML (e.g. rows 1 and 5 present, adding formulas
to rows 2:3 produced [1, 5, 2, 3]).
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      "xlsx_add_column.py")

NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = ("http://schemas.openxmlformats.org/officeDocument/2006/"
          "relationships")


def _tag(local: str) -> str:
    return f"{{{NS_SS}}}{local}"


WORKBOOK_XML = (
    '<?xml version="1.0" encoding="UTF-8"?>'
    f'<workbook xmlns="{NS_SS}" xmlns:r="{NS_REL}">'
    '<sheets><sheet name="S1" sheetId="1" r:id="rId1"/></sheets>'
    '</workbook>'
)

RELS_XML = (
    '<?xml version="1.0" encoding="UTF-8"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/'
    'package/2006/relationships">'
    '<Relationship Id="rId1" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/'
    'relationships/worksheet" Target="/xl/worksheets/sheet1.xml"/>'
    '</Relationships>'
)

SHARED_XML = (
    '<?xml version="1.0" encoding="UTF-8"?>'
    f'<sst xmlns="{NS_SS}" count="0" uniqueCount="0"/>'
)


def sheet_xml(rows):
    row_els = "".join(
        f'<row r="{r}"><c r="A{r}"><v>1</v></c>'
        f'<c r="B{r}"><v>2</v></c></row>'
        for r in rows
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        f'<worksheet xmlns="{NS_SS}" xmlns:r="{NS_REL}">'
        '<dimension ref="A1:B5"/>'
        f'<sheetData>{row_els}</sheetData>'
        '</worksheet>'
    )


class AddColumnRowOrderTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.work_dir = self.tmp.name
        xl = os.path.join(self.work_dir, "xl")
        os.makedirs(os.path.join(xl, "_rels"))
        os.makedirs(os.path.join(xl, "worksheets"))
        self._write(os.path.join(xl, "workbook.xml"), WORKBOOK_XML)
        self._write(os.path.join(xl, "_rels", "workbook.xml.rels"), RELS_XML)
        self._write(os.path.join(xl, "sharedStrings.xml"), SHARED_XML)
        self.sheet_path = os.path.join(xl, "worksheets", "sheet1.xml")

    def tearDown(self):
        self.tmp.cleanup()

    @staticmethod
    def _write(path, content):
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(content)

    def run_script(self, *extra):
        cmd = [sys.executable, SCRIPT, self.work_dir,
               "--col", "C", "--sheet", "S1", *extra]
        proc = subprocess.run(cmd, capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0,
                         msg=proc.stdout + proc.stderr)
        return proc

    def row_numbers(self):
        root = ET.parse(self.sheet_path).getroot()
        return [int(el.get("r")) for el in root.find(_tag("sheetData"))]

    def cell_refs(self):
        root = ET.parse(self.sheet_path).getroot()
        refs = []
        for row_el in root.find(_tag("sheetData")):
            for c in row_el:
                refs.append(c.get("r"))
        return refs

    def test_formula_rows_inserted_in_ascending_order(self):
        # Rows 1 and 5 exist; adding formulas to rows 2:3 must insert the
        # new rows between them, not append after row 5 (issue #3745).
        self._write(self.sheet_path, sheet_xml([1, 5]))
        self.run_script("--formula", "=A{row}+B{row}",
                        "--formula-rows", "2:3")
        self.assertEqual(self.row_numbers(), [1, 2, 3, 5])
        for ref in ("C2", "C3"):
            self.assertIn(ref, self.cell_refs())

    def test_total_row_inserted_in_sorted_position(self):
        self._write(self.sheet_path, sheet_xml([1, 5]))
        self.run_script("--formula", "=A{row}+B{row}",
                        "--formula-rows", "2:3",
                        "--total-row", "4",
                        "--total-formula", "=SUM(C2:C3)")
        self.assertEqual(self.row_numbers(), [1, 2, 3, 4, 5])
        self.assertIn("C4", self.cell_refs())

    def test_new_row_after_all_existing_rows(self):
        # Tail-insert branch of the helper: a batch of ascending inserts
        # (rows 2:5 over an existing row 1) must hit the fast path on
        # every iteration — with n growing per insert — and land at the
        # tail in order. The total row below the span (the documented
        # --formula-rows 2:9 --total-row 10 shape) covers the second
        # call site on the same fast path.
        self._write(self.sheet_path, sheet_xml([1]))
        self.run_script("--formula", "=A{row}+B{row}",
                        "--formula-rows", "2:5",
                        "--total-row", "6", "--total-formula",
                        "=SUM(C2:C5)")
        self.assertEqual(self.row_numbers(), [1, 2, 3, 4, 5, 6])
        for ref in ("C2", "C3", "C4", "C5", "C6"):
            self.assertIn(ref, self.cell_refs())

    def test_new_row_before_all_existing_rows(self):
        # Head-insert branch: existing [2,3] + formula on row 1 must land
        # before row 2, not at the tail.
        self._write(self.sheet_path, sheet_xml([2, 3]))
        self.run_script("--formula", "=A{row}+B{row}",
                        "--formula-rows", "1:1")
        self.assertEqual(self.row_numbers(), [1, 2, 3])
        self.assertIn("C1", self.cell_refs())

    def test_existing_rows_keep_order(self):
        # When all target rows already exist, no rows are created and the
        # original order is preserved.
        self._write(self.sheet_path, sheet_xml([1, 2, 3]))
        self.run_script("--formula", "=A{row}+B{row}",
                        "--formula-rows", "2:3")
        self.assertEqual(self.row_numbers(), [1, 2, 3])
        self.assertIn("C2", self.cell_refs())


if __name__ == "__main__":
    unittest.main(verbosity=2)
