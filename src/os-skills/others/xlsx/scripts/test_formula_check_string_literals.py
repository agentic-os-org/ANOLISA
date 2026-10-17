#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for formula_check.py string-literal handling.

formula_check used to scan raw formula text for sheet and name references, so
a string literal such as =IF(B1="Missing!x","Total",0) was reported as a
broken cross-sheet reference and the workbook was rejected. These tests pin
that string literals (including doubled-quote escapes) are ignored while real
references are still checked.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "formula_check.py")

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>
"""

RELS_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1"
    Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
    Target="worksheets/sheet1.xml"/>
</Relationships>
"""

CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" '
    'ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    "</Types>"
)


def _xml_escape(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def build_xlsx(path: str, formulas: list[tuple[str, str]]) -> str:
    rows = "".join(
        f'<row r="{cell[1:]}"><c r="{cell}"><f>{_xml_escape(formula)}</f>'
        f"<v>0</v></c></row>"
        for cell, formula in formulas
    )
    worksheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<worksheet xmlns="{NS_MAIN}"><sheetData>{rows}</sheetData></worksheet>'
    )
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("[Content_Types].xml", CONTENT_TYPES)
        archive.writestr("xl/workbook.xml", WORKBOOK_XML)
        archive.writestr("xl/_rels/workbook.xml.rels", RELS_XML)
        archive.writestr("xl/worksheets/sheet1.xml", worksheet)
    return path


def run_check(path: str) -> subprocess.CompletedProcess:
    environment = dict(os.environ, PYTHONIOENCODING="utf-8")
    return subprocess.run(
        [sys.executable, SCRIPT, path],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=environment,
    )


class TestFormulaCheckStringLiterals(unittest.TestCase):
    def test_string_literal_with_bang_is_not_a_sheet_ref(self):
        """Text inside a literal must not be reported as a missing sheet."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "literal.xlsx"),
                [("A1", 'IF(B1="Missing!x","Total",0)')],
            )
            result = run_check(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("PASS", result.stdout)
            self.assertNotIn("missing sheet", result.stdout)

    def test_bare_string_literal_passes(self):
        """A formula that is only a string literal is valid."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(os.path.join(root, "bare.xlsx"), [("A1", '"Warning!"')])
            result = run_check(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)

    def test_doubled_quote_escape_is_handled(self):
        """An escaped quote inside a literal must keep the literal intact."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "escaped.xlsx"),
                [("A1", '"say ""Missing!"" now"')],
            )
            result = run_check(xlsx)
            self.assertEqual(result.returncode, 0, result.stdout)

    def test_real_broken_ref_still_fails(self):
        """A genuine reference outside a literal must still be rejected."""
        with tempfile.TemporaryDirectory() as root:
            xlsx = build_xlsx(
                os.path.join(root, "broken.xlsx"),
                [("A1", '="ok"&Missing!A1')],
            )
            result = run_check(xlsx)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("missing sheet 'Missing'", result.stdout)


if __name__ == "__main__":
    unittest.main()
