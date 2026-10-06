#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py worksheet loading.

Regression test: workbook.xml.rels may use package-absolute targets
(Target="/xl/worksheets/sheet1.xml"). get_sheet_files used to keep the
leading "/", so the part never matched the zip namelist and check()
audited zero sheets — reporting PASS on a broken workbook.
"""

import os
import sys
import tempfile
import unittest
import zipfile

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

from formula_check import check  # noqa: E402

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
WS_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"

_CONTENT_TYPES = (
    '<?xml version="1.0"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    "</Types>"
)
_ROOT_RELS = (
    '<?xml version="1.0"?>'
    f'<Relationships xmlns="{NS_REL}">'
    '<Relationship Id="rId1" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" '
    'Target="xl/workbook.xml"/></Relationships>'
)
_WB = (
    '<?xml version="1.0"?>'
    f'<workbook xmlns="{NS}" xmlns:r="{NS_REL}">'
    '<sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>'
)
_WS_WITH_ERROR = (
    '<?xml version="1.0"?>'
    f'<worksheet xmlns="{NS}"><sheetData>'
    '<row r="1"><c r="A1" t="e"><f>B1+C1</f><v>#REF!</v></c></row>'
    "</sheetData></worksheet>"
)


def _make_xlsx(target: str) -> str:
    wb_rels = (
        '<?xml version="1.0"?>'
        f'<Relationships xmlns="{NS_REL}">'
        f'<Relationship Id="rId1" Type="{WS_TYPE}" Target="{target}"/></Relationships>'
    )
    fd, path = tempfile.mkstemp(suffix=".xlsx")
    os.close(fd)
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("[Content_Types].xml", _CONTENT_TYPES)
        z.writestr("_rels/.rels", _ROOT_RELS)
        z.writestr("xl/workbook.xml", _WB)
        z.writestr("xl/_rels/workbook.xml.rels", wb_rels)
        z.writestr("xl/worksheets/sheet1.xml", _WS_WITH_ERROR)
    return path


class TestPackageAbsoluteTargets(unittest.TestCase):
    def test_absolute_target_sheets_are_audited(self):
        results = check(_make_xlsx("/xl/worksheets/sheet1.xml"))
        self.assertEqual(results["sheets_checked"], ["Data"])
        self.assertGreaterEqual(results["error_count"], 1)

    def test_relative_target_sheets_are_audited(self):
        results = check(_make_xlsx("worksheets/sheet1.xml"))
        self.assertEqual(results["sheets_checked"], ["Data"])
        self.assertGreaterEqual(results["error_count"], 1)


if __name__ == "__main__":
    unittest.main()
