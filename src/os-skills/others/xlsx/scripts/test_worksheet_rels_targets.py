#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for worksheet resolution from workbook relationships.

OPC resolves a relationship Target against the directory of the part
that owns the rels file. openpyxl writes absolute part names
("/xl/worksheets/sheet1.xml") and the scripts handled that form, but
Excel and LibreOffice write relative targets ("worksheets/sheet1.xml"),
which used to be joined against the package root — missing the xl/
prefix — so every edit script crashed with FileNotFoundError on
Excel-authored workbooks.
"""

import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
INSERT_ROW = os.path.join(SCRIPTS_DIR, "xlsx_insert_row.py")
ADD_COLUMN = os.path.join(SCRIPTS_DIR, "xlsx_add_column.py")

NS_SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def build_work_dir(root, relative_target: bool):
    os.makedirs(os.path.join(root, "xl", "worksheets"))
    os.makedirs(os.path.join(root, "xl", "_rels"))
    target = (
        "worksheets/sheet1.xml" if relative_target
        else "/xl/worksheets/sheet1.xml"
    )
    with open(os.path.join(root, "xl", "workbook.xml"), "w") as fh:
        fh.write(
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            f'<workbook xmlns="{NS_SS}" xmlns:r="{NS_REL}">'
            f'<sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets>'
            f"</workbook>\n"
        )
    with open(
        os.path.join(root, "xl", "_rels", "workbook.xml.rels"), "w"
    ) as fh:
        fh.write(
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">\n'
            '  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
            f'Target="{target}"/>\n'
            "</Relationships>\n"
        )
    with open(
        os.path.join(root, "xl", "worksheets", "sheet1.xml"), "w"
    ) as fh:
        fh.write(
            f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            f'<worksheet xmlns="{NS_SS}"><sheetData>'
            f'<row r="1"><c r="A1"><v>10</v></c></row>'
            f'<row r="2"><c r="A2"><v>20</v></c></row>'
            f"</sheetData></worksheet>\n"
        )


class WorksheetRelsTargetTest(unittest.TestCase):

    def run_insert_row(self, work):
        return subprocess.run(
            [sys.executable, INSERT_ROW, work, "--at", "2", "--values", "B=99"],
            capture_output=True, text=True,
        )

    def run_add_column(self, work):
        return subprocess.run(
            [sys.executable, ADD_COLUMN, work, "--col", "B", "--formula",
             "=A{row}*2", "--formula-rows", "1:2"],
            capture_output=True, text=True,
        )

    def test_insert_row_resolves_the_excel_relative_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            work = os.path.join(tmp, "work")
            build_work_dir(work, relative_target=True)
            result = self.run_insert_row(work)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("Traceback", result.stderr)

    def test_add_column_resolves_the_excel_relative_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            work = os.path.join(tmp, "work")
            build_work_dir(work, relative_target=True)
            result = self.run_add_column(work)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn("Traceback", result.stderr)

    def test_absolute_targets_still_resolve(self):
        # openpyxl's form (absolute part name) must keep working.
        with tempfile.TemporaryDirectory() as tmp:
            work = os.path.join(tmp, "work")
            build_work_dir(work, relative_target=False)
            result = self.run_insert_row(work)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
