#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for formula_check.py worksheet relationship resolution.

Builds minimal in-memory workbooks (stdlib zipfile + ElementTree) whose
workbook rels exercise every OPC Target form, each carrying a deliberate
``#REF!`` error cell and a broken cross-sheet formula so the tests prove the
sheet is actually inspected — the defect in issue #3597 made these sheets
silently skip every check and report a clean result.
"""

from __future__ import annotations

import io
import tempfile
import unittest
import zipfile
import xml.etree.ElementTree as ET
from pathlib import Path

import formula_check

PKG_REL_NS = "http://schemas.openxmlformats.org/package/2006/relationships"
DOC_REL_NS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS = formula_check.NS

WORKSHEET_REL_TYPE = (
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
)
MAIN_CT = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
WS_CT = "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"

# Every OPC Target form seen in the wild for workbook -> worksheet rels.
ALL_TARGET_FORMS = {
    "package-absolute": "/xl/worksheets/sheet1.xml",
    "workbook-relative": "worksheets/sheet1.xml",
    "dot-segment": "./worksheets/sheet1.xml",
    "up-and-down": "../xl/worksheets/sheet1.xml",
    "prefixed": "xl/worksheets/sheet1.xml",
}


def build_workbook(rel_target: str) -> bytes:
    """Build a one-sheet xlsx whose single workbook rel uses rel_target.

    The sheet contains:
      - A1: t="e" error cell with #REF!  (Check 1: error-value cells)
      - B1: =Missing!A1                  (Check 2: broken cross-sheet ref)
    so a correctly resolved sheet always reports exactly 2 errors.
    """
    ct = ET.Element(
        "Types", {"xmlns": "http://schemas.openxmlformats.org/package/2006/content-types"}
    )
    ET.SubElement(ct, "Default", {"Extension": "rels", "ContentType": PKG_REL_NS + "+xml"})
    ET.SubElement(ct, "Default", {"Extension": "xml", "ContentType": "application/xml"})
    ET.SubElement(ct, "Override", {"PartName": "/xl/workbook.xml", "ContentType": MAIN_CT})
    ET.SubElement(ct, "Override", {"PartName": "/xl/worksheets/sheet1.xml", "ContentType": WS_CT})

    wb = ET.Element("workbook", {"xmlns": NS, "xmlns:r": DOC_REL_NS})
    sheets = ET.SubElement(wb, "sheets")
    ET.SubElement(sheets, "sheet", {"name": "Data", "sheetId": "1", f"{{{DOC_REL_NS}}}id": "rId1"})

    rels = ET.Element("Relationships", {"xmlns": PKG_REL_NS})
    ET.SubElement(
        rels,
        "Relationship",
        {"Id": "rId1", "Type": WORKSHEET_REL_TYPE, "Target": rel_target},
    )

    ws = ET.Element("worksheet", {"xmlns": NS})
    data = ET.SubElement(ws, "sheetData")
    row = ET.SubElement(data, "row", {"r": "1"})
    a1 = ET.SubElement(row, "c", {"r": "A1", "t": "e"})
    ET.SubElement(a1, "v").text = "#REF!"
    b1 = ET.SubElement(row, "c", {"r": "B1"})
    ET.SubElement(b1, "f").text = "Missing!A1"

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr("[Content_Types].xml", ET.tostring(ct, encoding="UTF-8", xml_declaration=True))
        z.writestr("xl/workbook.xml", ET.tostring(wb, encoding="UTF-8", xml_declaration=True))
        z.writestr(
            "xl/_rels/workbook.xml.rels", ET.tostring(rels, encoding="UTF-8", xml_declaration=True)
        )
        z.writestr(
            "xl/worksheets/sheet1.xml", ET.tostring(ws, encoding="UTF-8", xml_declaration=True)
        )
    return buf.getvalue()


class ResolveRelTargetTest(unittest.TestCase):
    """Unit-test the target -> member resolution helper directly."""

    def test_package_absolute_target_resolves_from_package_root(self):
        self.assertEqual(
            formula_check.resolve_rel_target("/xl/worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml",
        )

    def test_workbook_relative_target_is_joined_onto_xl(self):
        self.assertEqual(
            formula_check.resolve_rel_target("worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml",
        )

    def test_prefixed_target_is_kept_as_is(self):
        self.assertEqual(
            formula_check.resolve_rel_target("xl/worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml",
        )

    def test_dot_segments_are_normalized(self):
        self.assertEqual(
            formula_check.resolve_rel_target("./worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml",
        )
        self.assertEqual(
            formula_check.resolve_rel_target("../xl/worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml",
        )

    def test_double_slash_prefix_is_not_double_joined(self):
        # Regression guard for the exact defect: naive joining produced
        # "xl//xl/worksheets/sheet1.xml" which never matched the archive.
        self.assertNotIn(
            "//",
            formula_check.resolve_rel_target("/xl/worksheets/sheet1.xml"),
        )


class SheetInspectionPerTargetFormTest(unittest.TestCase):
    """Every Target form must resolve so the sheet's errors are found."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)

    def check_workbook(self, rel_target: str) -> dict:
        path = str(Path(self._tmp.name) / "wb.xlsx")
        with open(path, "wb") as f:
            f.write(build_workbook(rel_target))
        return formula_check.check(path)

    def test_sheet_behind_each_target_form_is_checked(self):
        for form, target in ALL_TARGET_FORMS.items():
            with self.subTest(form=form, target=target):
                results = self.check_workbook(target)
                self.assertEqual(results["sheets_checked"], ["Data"])
                self.assertEqual(results["formula_count"], 1)
                error_types = {e["type"] for e in results["errors"]}
                self.assertEqual(error_types, {"error_value", "broken_sheet_ref"})
                self.assertEqual(results["error_count"], 2)

    def test_package_absolute_target_sheet_errors_reported(self):
        # The exact reproduction from the issue: with Target="/xl/worksheets/
        # sheet1.xml" the old code reported sheets_checked=[] and
        # error_count=0, hiding the #REF! cell.
        results = self.check_workbook("/xl/worksheets/sheet1.xml")
        self.assertIn("Data", results["sheets_checked"])
        error_values = [
            e for e in results["errors"] if e["type"] == "error_value" and e["error"] == "#REF!"
        ]
        self.assertEqual(len(error_values), 1)
        self.assertEqual(error_values[0]["cell"], "A1")

    def test_sheet_filter_still_applies(self):
        # A filter matching nothing checks nothing, regardless of target form.
        results = self.check_workbook("/xl/worksheets/sheet1.xml")
        self.assertIn("Data", results["sheets_checked"])
        empty = formula_check.check(
            str(Path(self._tmp.name) / "wb.xlsx"), sheet_filter="NoSuchSheet"
        )
        self.assertEqual(empty["sheets_checked"], [])

    def test_multiple_sheets_mixed_target_forms(self):
        # Two sheets, one behind a package-absolute target and one behind a
        # workbook-relative target: both must be inspected.
        path = str(Path(self._tmp.name) / "mixed.xlsx")
        ct = ET.Element(
            "Types", {"xmlns": "http://schemas.openxmlformats.org/package/2006/content-types"}
        )
        ET.SubElement(ct, "Default", {"Extension": "rels", "ContentType": PKG_REL_NS + "+xml"})
        ET.SubElement(ct, "Default", {"Extension": "xml", "ContentType": "application/xml"})
        ET.SubElement(ct, "Override", {"PartName": "/xl/workbook.xml", "ContentType": MAIN_CT})
        ET.SubElement(ct, "Override", {"PartName": "/xl/worksheets/sheet1.xml", "ContentType": WS_CT})
        ET.SubElement(ct, "Override", {"PartName": "/xl/worksheets/sheet2.xml", "ContentType": WS_CT})

        wb = ET.Element("workbook", {"xmlns": NS, "xmlns:r": DOC_REL_NS})
        sheets = ET.SubElement(wb, "sheets")
        ET.SubElement(sheets, "sheet", {"name": "Abs", "sheetId": "1", f"{{{DOC_REL_NS}}}id": "rId1"})
        ET.SubElement(sheets, "sheet", {"name": "Rel", "sheetId": "2", f"{{{DOC_REL_NS}}}id": "rId2"})

        rels = ET.Element("Relationships", {"xmlns": PKG_REL_NS})
        ET.SubElement(
            rels, "Relationship", {"Id": "rId1", "Type": WORKSHEET_REL_TYPE,
                                   "Target": "/xl/worksheets/sheet1.xml"}
        )
        ET.SubElement(
            rels, "Relationship", {"Id": "rId2", "Type": WORKSHEET_REL_TYPE,
                                   "Target": "worksheets/sheet2.xml"}
        )

        def sheet_xml(cell_ref: str) -> bytes:
            ws = ET.Element("worksheet", {"xmlns": NS})
            data = ET.SubElement(ws, "sheetData")
            row = ET.SubElement(data, "row", {"r": "1"})
            c = ET.SubElement(row, "c", {"r": cell_ref, "t": "e"})
            ET.SubElement(c, "v").text = "#DIV/0!"
            return ET.tostring(ws, encoding="UTF-8", xml_declaration=True)

        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            z.writestr("[Content_Types].xml", ET.tostring(ct, encoding="UTF-8", xml_declaration=True))
            z.writestr("xl/workbook.xml", ET.tostring(wb, encoding="UTF-8", xml_declaration=True))
            z.writestr(
                "xl/_rels/workbook.xml.rels",
                ET.tostring(rels, encoding="UTF-8", xml_declaration=True),
            )
            z.writestr("xl/worksheets/sheet1.xml", sheet_xml("A1"))
            z.writestr("xl/worksheets/sheet2.xml", sheet_xml("A1"))
        with open(path, "wb") as f:
            f.write(buf.getvalue())

        results = formula_check.check(path)
        self.assertEqual(sorted(results["sheets_checked"]), ["Abs", "Rel"])
        self.assertEqual(results["error_count"], 2)


if __name__ == "__main__":
    unittest.main()
