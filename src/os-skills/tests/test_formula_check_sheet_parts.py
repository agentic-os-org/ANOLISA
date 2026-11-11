#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py worksheet part resolution and fail-closed checking.

A `workbook.xml.rels` Target may be relative to `xl/` (`worksheets/sheet1.xml`),
dot-segmented (`./worksheets/sheet1.xml`), or package-absolute
(`/xl/worksheets/sheet1.xml`, which openpyxl 3.1 writes). Only the first form
resolved, so the other two produced member names absent from the archive, the
sheet was skipped, and the validator still printed
`PASS — No formula errors detected` after checking zero formulas.
"""

import os
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile

sys.dont_write_bytecode = True

sys.path.insert(
    0,
    str(Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"),
)
import formula_check as fc  # noqa: E402

CONTENT_TYPES = (
    '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/'
    'package/2006/content-types"><Default Extension="rels" ContentType='
    '"application/vnd.openxmlformats-package.relationships+xml"/><Default '
    'Extension="xml" ContentType="application/xml"/><Override '
    'PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-'
    'officedocument.spreadsheetml.sheet.main+xml"/><Override '
    'PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.'
    'openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>'
)
ROOT_RELS = (
    '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.'
    'org/package/2006/relationships"><Relationship Id="rId1" Type="http://'
    'schemas.openxmlformats.org/officeDocument/2006/relationships/office'
    'Document" Target="xl/workbook.xml"/></Relationships>'
)
WORKBOOK = (
    '<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/'
    'spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/'
    'officeDocument/2006/relationships"><sheets><sheet name="Data" '
    'sheetId="1" r:id="rId1"/></sheets></workbook>'
)


def workbook_rels(target: str) -> str:
    return (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.'
        'openxmlformats.org/package/2006/relationships"><Relationship '
        'Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/'
        '2006/relationships/worksheet" Target="' + target + '"/></Relationships>'
    )


def worksheet(formula: str) -> str:
    return (
        '<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.'
        'org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1">'
        '<f>' + formula + '</f><v>1</v></c></row></sheetData></worksheet>'
    )


def build(path: str, target: str, formula: str = "B1+1", part: str = "xl/worksheets/sheet1.xml"):
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("_rels/.rels", ROOT_RELS)
        z.writestr("xl/workbook.xml", WORKBOOK)
        z.writestr("xl/_rels/workbook.xml.rels", workbook_rels(target))
        z.writestr(part, worksheet(formula))
    return path


class TestResolveSheetTarget(unittest.TestCase):
    def test_relative_target(self):
        self.assertEqual(fc.resolve_sheet_target("worksheets/sheet1.xml"),
                         "xl/worksheets/sheet1.xml")

    def test_package_absolute_target(self):
        """openpyxl 3.1 writes /xl/worksheets/sheet1.xml; it must not become xl//xl/…"""
        self.assertEqual(fc.resolve_sheet_target("/xl/worksheets/sheet1.xml"),
                         "xl/worksheets/sheet1.xml")

    def test_dot_segment_target(self):
        self.assertEqual(fc.resolve_sheet_target("./worksheets/sheet1.xml"),
                         "xl/worksheets/sheet1.xml")

    def test_parent_segment_target(self):
        self.assertEqual(fc.resolve_sheet_target("./worksheets/../worksheets/sheet1.xml"),
                         "xl/worksheets/sheet1.xml")


class TestSheetParts(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.path = os.path.join(self._tmp.name, "book.xlsx")

    def test_every_target_form_checks_the_sheet(self):
        """Both alt forms must reach the worksheet, not skip it."""
        for target in ("worksheets/sheet1.xml",
                       "/xl/worksheets/sheet1.xml",
                       "./worksheets/sheet1.xml"):
            with self.subTest(target=target):
                build(self.path, target, formula="1+1")
                result = fc.check(self.path)
                self.assertEqual(result["sheets_checked"], ["Data"])
                self.assertEqual(result["formula_count"], 1)
                self.assertEqual(result["error_count"], 0)

    def test_bad_formula_is_caught_behind_absolute_target(self):
        """The reported symptom: a broken reference reported PASS."""
        build(self.path, "/xl/worksheets/sheet1.xml", formula="Missing!A1+1")
        result = fc.check(self.path)

        self.assertEqual(result["sheets_checked"], ["Data"])
        self.assertEqual(result["error_count"], 1)
        self.assertEqual(result["errors"][0]["type"], "broken_sheet_ref")

    def test_missing_sheet_part_fails_closed(self):
        """A declared sheet with no readable part must not be a silent skip."""
        build(self.path, "worksheets/sheet9.xml", formula="1+1",
              part="xl/worksheets/sheet1.xml")
        result = fc.check(self.path)

        self.assertEqual(result["sheets_checked"], [])
        self.assertEqual(result["error_count"], 1)
        self.assertEqual(result["errors"][0]["type"], "missing_sheet_part")
        self.assertIn("sheet9.xml", result["errors"][0]["message"])


if __name__ == "__main__":
    unittest.main()
