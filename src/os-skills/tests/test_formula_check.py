#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""String literals must not be read as sheet or name references.

formula_check scans raw formula text, so text inside a double-quoted literal was
extracted as a reference: `=IF(A1="Missing!x","Total",0)` failed with
`references missing sheet 'Missing'` (a hard broken_sheet_ref error) and
`="Warning!"` reported a missing sheet `Warning`.
"""

import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import formula_check

sys.dont_write_bytecode = True

SCRIPT = SCRIPTS_DIR / "formula_check.py"
NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-'
    'package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-'
    'officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.'
    'openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>'
)
PACKAGE_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    f'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    f'<Relationship Id="rId1" Type="{NS_REL}/officeDocument" Target="xl/workbook.xml"/>'
    "</Relationships>"
)
WORKBOOK = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">'
    '<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>'
)
WORKBOOK_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    f'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    f'<Relationship Id="rId1" Type="{NS_REL}/worksheet" Target="worksheets/sheet1.xml"/>'
    "</Relationships>"
)


def build_workbook(path, formulas):
    """Write a one-sheet workbook whose A-column cells carry `formulas`."""
    rows = "".join(
        f'<row r="{index}"><c r="A{index}"><f>{formula}</f><v>0</v></c></row>'
        for index, formula in enumerate(formulas, start=1)
    )
    sheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<worksheet xmlns="{NS_MAIN}"><dimension ref="A1:A{len(formulas)}"/>'
        f"<sheetData>{rows}</sheetData></worksheet>"
    )
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("[Content_Types].xml", CONTENT_TYPES)
        archive.writestr("_rels/.rels", PACKAGE_RELS)
        archive.writestr("xl/workbook.xml", WORKBOOK)
        archive.writestr("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        archive.writestr("xl/worksheets/sheet1.xml", sheet)


class TestStringLiteralsAreNotReferences(unittest.TestCase):
    def test_literal_argument_of_if_is_not_a_sheet(self):
        self.assertEqual(formula_check.extract_sheet_refs('IF(A1="Missing!x","Total",0)'), [])

    def test_literal_only_formula_is_not_a_sheet(self):
        self.assertEqual(formula_check.extract_sheet_refs('"Warning!"'), [])

    def test_doubled_quote_escape_is_handled(self):
        self.assertEqual(
            formula_check.extract_sheet_refs('"a""Missing!b"&Data!A1'),
            ["Data"],
        )

    def test_real_sheet_reference_is_still_found(self):
        self.assertEqual(formula_check.extract_sheet_refs("Data!A1"), ["Data"])
        self.assertEqual(formula_check.extract_sheet_refs("'My Sheet'!A1"), ["My Sheet"])

    def test_literal_is_not_read_as_a_defined_name(self):
        self.assertEqual(formula_check.extract_name_refs('IF(A1="MyName",1,2)'), [])


class TestCliPassesOnStringLiterals(unittest.TestCase):
    def test_workbook_with_literal_only_formulas_passes(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = Path(tmpdir) / "strlit.xlsx"
            build_workbook(path, ['"Warning!"', 'IF(A1="Missing!x","Total",0)'])

            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(path)],
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PASS", result.stdout)


if __name__ == "__main__":
    unittest.main()
