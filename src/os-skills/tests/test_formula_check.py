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

# Must precede the local import below, or the interpreter writes a .pyc for it
# before the flag is set.
sys.dont_write_bytecode = True

SCRIPTS_DIR = Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import formula_check  # noqa: E402

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
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
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
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
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


class TestQuotedSheetNamesHoldTheirQuotes(unittest.TestCase):
    """A quoted sheet name is not a string literal.

    `SUM('Q3 "Final"'!A1:A9)` refers to a sheet whose name contains double
    quotes. Stripping every `"..."` span cut the name at its own quotes and left
    `Q3 ` behind, which was then reported as a missing sheet.
    """

    def test_a_quoted_sheet_name_keeps_its_double_quotes(self):
        self.assertEqual(
            formula_check.extract_sheet_refs('SUM(\'Q3 "Final"\'!A1:A9)'),
            ['Q3 "Final"'],
        )

    def test_the_quoted_name_survives_next_to_a_real_literal(self):
        formula = 'IF(A1="Missing!x",SUM(\'Q3 "Final"\'!A1:A9),0)'

        stripped = formula_check.strip_string_literals(formula)

        self.assertIn('Q3 "Final"', stripped)
        self.assertNotIn("Missing", stripped)
        self.assertEqual(formula_check.extract_sheet_refs(formula), ['Q3 "Final"'])

    def test_a_doubled_quote_inside_a_literal_is_still_one_literal(self):
        self.assertEqual(formula_check.extract_sheet_refs('IF(A1="a""b",X!B1)'), ["X"])

    def test_an_escaped_apostrophe_in_a_sheet_name_is_one_name(self):
        self.assertEqual(formula_check.extract_sheet_refs("SUM('It''s'!A1)"), ["It's"])

    def test_a_double_quote_in_a_name_does_not_swallow_a_real_ref(self):
        # The formula holds an odd number of `"`, so any regex over `"..."` pairs
        # the name's own quote with a literal elsewhere and deletes the real
        # reference instead of reporting it. This is the miss side of the bug the
        # scanner replaced: every other case here covers a false report.
        formula = 'IF(A1="x",\'O"Brien\'!B2)&"y"'

        self.assertEqual(formula_check.extract_sheet_refs(formula), ['O"Brien'])

    def test_an_unclosed_literal_keeps_the_rest_of_the_formula(self):
        # A truncated `<f>` is a usual failure in machine-generated workbooks. The
        # remaining text must still be checked rather than dropped silently, so an
        # unclosed span is kept verbatim instead of discarded.
        self.assertEqual(formula_check.extract_sheet_refs('"Missing!A1'), ["Missing"])

    def test_the_two_quote_kinds_agree_on_an_unclosed_span(self):
        # Before this, `'` kept an unclosed tail and `"` dropped it, which made the
        # same malformed input pass or fail depending on which quote opened it.
        for formula in ("'Missing!A1", '"Missing!A1'):
            with self.subTest(formula=formula):
                self.assertEqual(
                    formula_check.strip_string_literals(formula), formula
                )


if __name__ == "__main__":
    unittest.main()
