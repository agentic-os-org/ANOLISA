#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for formula_check.py relationship resolution.

Run from this directory:
    python3 -m unittest test_formula_check -v
"""

import io
import os
import sys
import unittest
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import formula_check as fc  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL_DOC = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"

WORKBOOK = (
    '<?xml version="1.0"?>'
    f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL_DOC}">'
    '<sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets>'
    "</workbook>"
)

# A worksheet carrying a real defect: an error-value cell that must be found
# whenever the sheet is actually inspected.
WORKSHEET = (
    '<?xml version="1.0"?>'
    f'<worksheet xmlns="{NS_MAIN}"><sheetData>'
    '<row r="1"><c r="A1" t="e"><v>#REF!</v></c></row>'
    "</sheetData></worksheet>"
)


def rels_for(target: str) -> str:
    return (
        '<?xml version="1.0"?>'
        f'<Relationships xmlns="{NS_PKG_REL}">'
        '<Relationship Id="rId1"'
        ' Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"'
        f' Target="{target}"/>'
        "</Relationships>"
    )


def build_xlsx(rels: str) -> str:
    """Write an in-memory workbook with the given rels and return its path."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr("xl/workbook.xml", WORKBOOK)
        z.writestr("xl/_rels/workbook.xml.rels", rels)
        z.writestr("xl/worksheets/sheet1.xml", WORKSHEET)
    import tempfile

    fd, path = tempfile.mkstemp(suffix=".xlsx")
    with os.fdopen(fd, "wb") as f:
        f.write(buf.getvalue())
    return path


class PackageAbsoluteTargetsTest(unittest.TestCase):
    def setUp(self):
        self.paths = []
        self.addCleanup(self._remove)

    def _remove(self):
        for p in self.paths:
            try:
                os.unlink(p)
            except FileNotFoundError:
                pass

    def _check(self, rels: str) -> dict:
        path = build_xlsx(rels)
        self.paths.append(path)
        return fc.check(path)

    def test_package_absolute_target_is_inspected(self):
        """/xl/worksheets/sheet1.xml must resolve, not double the xl/ prefix."""
        results = self._check(rels_for("/xl/worksheets/sheet1.xml"))
        self.assertEqual(results["sheets_checked"], ["Sales"])
        self.assertEqual(results["error_count"], 1)
        self.assertEqual(results["errors"][0]["type"], "error_value")

    def test_relative_target_still_resolves(self):
        results = self._check(rels_for("worksheets/sheet1.xml"))
        self.assertEqual(results["sheets_checked"], ["Sales"])
        self.assertEqual(results["error_count"], 1)

    def test_dot_segment_target_resolves(self):
        results = self._check(rels_for("./worksheets/sheet1.xml"))
        self.assertEqual(results["sheets_checked"], ["Sales"])
        self.assertEqual(results["error_count"], 1)

    def test_up_down_target_resolves(self):
        ""'"../xl/worksheets/sheet1.xml" must normalize back into xl/.'''
        results = self._check(rels_for("../xl/worksheets/sheet1.xml"))
        self.assertEqual(results["sheets_checked"], ["Sales"])
        self.assertEqual(results["error_count"], 1)


if __name__ == "__main__":
    unittest.main()
