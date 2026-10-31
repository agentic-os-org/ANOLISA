#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py worksheet rel target resolution.

Regression tests for package-absolute worksheet targets: openpyxl writes
Target="/xl/worksheets/sheet1.xml" (package-rooted). The resolver used to
prefix such targets with "xl/" and produce "xl//xl/worksheets/sheet1.xml",
which matches no zip member, so every sheet was silently skipped and the
validator printed PASS without checking a single formula. The same
absolute-target form is already handled by the other edit scripts
(style_audit, xlsx_insert_row, xlsx_add_column).
"""

import os
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formula_check as fc  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"

# One definite error the validator must find: a t="e" cell holding #REF!.
WORKSHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <sheetData>
    <row r="1"><c r="A1" t="e"><v>#REF!</v></c></row>
  </sheetData>
</worksheet>
"""

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"""


def build_workbook(path: str, worksheet_target: str) -> str:
    """Write a minimal workbook whose worksheet rel uses the given target."""
    rels_xml = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<Relationships xmlns="{NS_PKG_REL}">'
        '<Relationship Id="rId1" '
        'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
        f'Target="{worksheet_target}"/>'
        "</Relationships>"
    )
    content_types = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        "</Types>"
    )
    root_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<Relationships xmlns="{NS_PKG_REL}">'
        '<Relationship Id="rId1" '
        'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" '
        'Target="xl/workbook.xml"/>'
        "</Relationships>"
    )
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", content_types)
        z.writestr("_rels/.rels", root_rels)
        z.writestr("xl/workbook.xml", WORKBOOK_XML)
        z.writestr("xl/_rels/workbook.xml.rels", rels_xml)
        z.writestr("xl/worksheets/sheet1.xml", WORKSHEET_XML)
    return path


class TestWorksheetTargetResolution(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.xlsx = os.path.join(self._tmp.name, "model.xlsx")

    def tearDown(self):
        self._tmp.cleanup()

    def test_absolute_target_sheets_are_checked(self):
        """openpyxl-style absolute targets must resolve, not silently skip.

        Before the fix the target became "xl//xl/worksheets/sheet1.xml",
        no zip member matched, the sheet was skipped, and a workbook whose
        only cell holds #REF! passed validation.
        """
        build_workbook(self.xlsx, "/xl/worksheets/sheet1.xml")
        results = fc.check(self.xlsx)
        self.assertIn("Data", results["sheets_checked"],
                      "the sheet must be checked")
        self.assertGreaterEqual(results["error_count"], 1,
                                "the #REF! cell must be reported")

    def test_relative_target_sheets_are_checked(self):
        """Excel-style relative targets keep working (control)."""
        build_workbook(self.xlsx, "worksheets/sheet1.xml")
        results = fc.check(self.xlsx)
        self.assertIn("Data", results["sheets_checked"])
        self.assertGreaterEqual(results["error_count"], 1)


if __name__ == "__main__":
    unittest.main()
