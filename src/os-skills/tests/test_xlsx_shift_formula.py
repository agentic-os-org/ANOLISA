# SPDX-License-Identifier: MIT
"""Tests for shift_formula reference boundaries in xlsx_shift_rows.py.

Non-cell tokens must survive row shifting untouched: function names
(LOG10, ATAN2, DAYS360, HEX2DEC, SUMX2MY2), scientific notation (1E5),
sheet prefixes (SHEET1!, Q1!), structured references (TBL1[#All], and
ref-shaped column names inside brackets like Sales[FY2025]), defined
names (Q1_TOTAL, Data.A1), _xlfn prefixes, and double-quoted string
literals ("Q3"). Real A1-style references (including $-anchored and
XFD-limit forms) still shift.

EndToEndShiftTest drives the real CLI over an unpacked worksheet so the
tokenizer is exercised through the insert path, not just by import.
Lives outside the skill directory so it is CI-discovered but never
shipped in the user-facing payload — but it imports FROM the payload
tree, so it must never write into it (the bytecode guard below holds
that invariant: a stray __pycache__/ under others/xlsx/scripts breaks
tests/test_raw_packaging.py's payload assertion).
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

# The scripts are standalone CLIs (not a package), so point the import
# path at src/os-skills/others/xlsx/scripts relative to this test file.
SCRIPTS_DIR = os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 os.pardir, "others", "xlsx", "scripts"))

# Importing from the payload tree would otherwise make CPython drop
# __pycache__/xlsx_shift_rows.cpython-*.pyc next to the script, i.e. inside
# the shipped xlsx skill directory. tests/test_raw_packaging.py builds its
# expected payload set from a filesystem walk while packaging/raw/package.py
# collects tracked files via `git ls-files`, and .gitignore hides __pycache__,
# so that stray .pyc fails the packaging assertion.
_PREV_DONT_WRITE_BYTECODE = sys.dont_write_bytecode
_INSERTED_SCRIPTS_DIR = SCRIPTS_DIR not in sys.path
try:
    sys.dont_write_bytecode = True
    if _INSERTED_SCRIPTS_DIR:
        sys.path.insert(0, SCRIPTS_DIR)

    from xlsx_shift_rows import shift_formula  # noqa: E402
finally:
    sys.dont_write_bytecode = _PREV_DONT_WRITE_BYTECODE
    if _INSERTED_SCRIPTS_DIR:
        # The module object stays bound after the path entry is gone;
        # removing it keeps this module from leaking 11 importable
        # top-level script names into every later test import.
        sys.path.remove(SCRIPTS_DIR)


class FunctionNameTest(unittest.TestCase):
    """Function names that end in digits must never be shifted."""

    def test_function_names_with_trailing_digits(self):
        cases = [
            ("LOG10(B2)", "LOG10(B3)"),
            ("ATAN2(A2,B2)", "ATAN2(A3,B3)"),
            ("DAYS360(A2,B2)", "DAYS360(A3,B3)"),
            ("HEX2DEC(A2)", "HEX2DEC(A3)"),
            ("DEC2BIN(A2)", "DEC2BIN(A3)"),
            ("SUMX2MY2(A2:A5,B2:B5)", "SUMX2MY2(A3:A6,B3:B6)"),
        ]
        for formula, expected in cases:
            with self.subTest(formula=formula):
                self.assertEqual(shift_formula(formula, 2, 1), expected)

    def test_function_name_alone_is_untouched(self):
        self.assertEqual(shift_formula("LOG10(1)", 1, 1), "LOG10(1)")
        # "10" inside LOG10( must not shift even when at <= 10
        self.assertEqual(shift_formula("LOG10(1)", 10, 1), "LOG10(1)")


class ScientificNotationTest(unittest.TestCase):
    """Numeric literals like 1E5 must not be shifted."""

    def test_scientific_notation_preserved(self):
        self.assertEqual(shift_formula("1E5+A1", 2, 1), "1E5+A1")
        self.assertEqual(shift_formula("1E5+A2", 1, 1), "1E5+A3")
        self.assertEqual(shift_formula("1.5E3*B2", 1, 1), "1.5E3*B3")


class SheetPrefixTest(unittest.TestCase):
    """Sheet prefixes survive; the cell refs after ! still shift."""

    def test_long_uppercase_sheet_prefix_preserved(self):
        # SHEET1 is protected by the {1,3} column-letter cap
        self.assertEqual(shift_formula("SHEET1!B7", 1, 1), "SHEET1!B8")

    def test_short_uppercase_sheet_prefix_preserved(self):
        # Q1 fits the cap, so only the "!" boundary guard protects it
        self.assertEqual(shift_formula("Q1!B5", 1, 2), "Q1!B7")
        self.assertEqual(shift_formula("Q1!B5:B7", 1, 2), "Q1!B7:B9")

    def test_quoted_sheet_name_preserved(self):
        # at=1 pins the quote protection: an unprotected tokenizer would
        # rewrite the quoted Q1/FY2025. at=5 would pass even without it.
        self.assertEqual(shift_formula("'Q1 Data'!B7", 1, 1), "'Q1 Data'!B8")
        self.assertEqual(
            shift_formula("'Budget FY2025'!A1", 1, 2), "'Budget FY2025'!A3")


class StructuredReferenceTest(unittest.TestCase):
    """Structured table references must never be shifted."""

    def test_structured_reference_preserved(self):
        self.assertEqual(shift_formula("TBL1[#All]", 1, 2), "TBL1[#All]")
        self.assertEqual(shift_formula("SUM(TBL1[Qty])", 1, 2), "SUM(TBL1[Qty])")

    def test_structured_reference_column_names_preserved(self):
        # Ref-shaped column names inside [...] look like cell refs but
        # belong to the table, so they must never be rewritten.
        cases = [
            ("Sales[FY2025]", "Sales[FY2025]"),
            ("Table1[Q1]", "Table1[Q1]"),
            ("TBL1[A5]", "TBL1[A5]"),
            ("TBL1[[#Totals],[FY2024]]", "TBL1[[#Totals],[FY2024]]"),
            ("SUM(Sales[FY2025])", "SUM(Sales[FY2025])"),
        ]
        for formula, expected in cases:
            with self.subTest(formula=formula):
                self.assertEqual(shift_formula(formula, 1, 2), expected)

    def test_structured_reference_mixed_with_real_refs(self):
        # Bracket interiors stay untouched while real refs around them
        # still shift.
        self.assertEqual(
            shift_formula("SUM(Table1[FY2025])+B5", 1, 2),
            "SUM(Table1[FY2025])+B7")

    def test_bracketed_column_with_escaped_quote(self):
        # A bracketed column name with an escaped quote ('' inside the
        # brackets) must not let a sheet-quote run swallow the real refs
        # after the closing bracket: with a later quote in the formula,
        # a sheet-quote-first pattern ('[^']*(?:''[^']*)*') would pair
        # with it and freeze everything in between. Pins the
        # escape-aware bracket alternative being tried BEFORE the
        # sheet-quote alternative.
        self.assertEqual(
            shift_formula("SUM(TBL1['It''s])+B5,'x')", 1, 2),
            "SUM(TBL1['It''s])+B7,'x')")
        self.assertEqual(
            shift_formula("TBL1[''])+B5+'S'!A1", 1, 2),
            "TBL1[''])+B7+'S'!A3")

    def test_bracketed_column_quote_without_pair(self):
        # Even without a later quote to pair with, the bracket interior
        # itself stays protected (Q5 would shift to Q6 without it) and
        # the ref after it still shifts.
        self.assertEqual(
            shift_formula("SUM(TBL1['Q5])+C5", 2, 1), "SUM(TBL1['Q5])+C6")

    def test_odd_trailing_quote_before_close_bracket(self):
        # A lone ' immediately before ] escapes that ], so a malformed
        # odd trailing quote leaves the interior treated as regular
        # text: Q5 inside shifts, and so does B5 after the bracket.
        # Excel's legal encoding of a trailing quote is '' — that form
        # stays protected. Pins the deliberate reading of the exclusive
        # bracket body.
        self.assertEqual(
            shift_formula("TBL1[Q5']+B5", 1, 2), "TBL1[Q7']+B7")
        self.assertEqual(
            shift_formula("TBL1[Q5'']+B5", 1, 2), "TBL1[Q5'']+B7")


class BacktrackingGuardTest(unittest.TestCase):
    """A failed bracket run must not backtrack exponentially.

    The bracket body's quote branches are mutually exclusive so each
    interior position has exactly one parse. An overlapping body (''
    and bare-' branches both consuming two apostrophes) made a failed
    bracket run retry every split of the apostrophes — the parse count
    grows Fibonacci-fast, and an unterminated "[" plus 40 apostrophes
    (a 55-char formula) hung the script through the real CLI. The
    guard runs shift_formula in a subprocess with a timeout because the
    overlapping form never returns: a plain elapsed-time assert could
    only fail after the hang, the timeout fails it in seconds.
    """

    def test_unterminated_bracket_apostrophes_return(self):
        for formula in ("[" + "'" * 48,
                        "SUM(Table1[" + "'" * 40 + ")+B5"):
            with self.subTest(formula=formula[:24]):
                code = (
                    "import sys; sys.dont_write_bytecode = True; "
                    "sys.path.insert(0, sys.argv[1]); "
                    "import xlsx_shift_rows as m; "
                    "m.shift_formula(sys.argv[2], 2, 1)"
                )
                # -B plus the in-code flag: never write bytecode into
                # the payload tree from this subprocess either.
                proc = subprocess.run(
                    [sys.executable, "-B", "-c", code, SCRIPTS_DIR, formula],
                    capture_output=True, text=True, timeout=10)
                self.assertEqual(proc.returncode, 0, proc.stderr)


class DefinedNameTest(unittest.TestCase):
    """Defined names (underscoreed or dotted) must never be shifted."""

    def test_underscore_name_preserved(self):
        self.assertEqual(shift_formula("Q1_TOTAL+A1", 1, 2), "Q1_TOTAL+A3")

    def test_underscore_suffix_name_preserved(self):
        # A defined name whose tail looks like a ref (SALES_A5) must not
        # have that tail shifted; pins the "_" in the lookbehind.
        self.assertEqual(shift_formula("SALES_A5+B1", 1, 2), "SALES_A5+B3")

    def test_mixed_case_name_preserved(self):
        # A ref-shaped token glued to lowercase letters is a name like
        # SalesQ1 or Q1total, not a cell ref: pins the a-z ranges in
        # both lookarounds, which no other assertion covers (dropping
        # a-z from either class fails zero tests but shifts Q1/Q5 in
        # names like these).
        self.assertEqual(shift_formula("SalesQ1+B5", 1, 2), "SalesQ1+B7")
        self.assertEqual(shift_formula("Q1total+A5", 1, 2), "Q1total+A7")

    def test_dotted_name_preserved(self):
        self.assertEqual(shift_formula("Data.A1", 1, 2), "Data.A1")

    def test_xlfn_prefix_preserved(self):
        self.assertEqual(shift_formula("_xlfn.A1", 1, 2), "_xlfn.A1")


class StringLiteralTest(unittest.TestCase):
    """Double-quoted string literals must never be shifted."""

    def test_comparison_literal_preserved(self):
        self.assertEqual(
            shift_formula('IF(A5="Q3",B5,C5)', 1, 2), 'IF(A7="Q3",B7,C7)')
        self.assertEqual(
            shift_formula('IF($A$5="FY2025",1,0)', 1, 2),
            'IF($A$7="FY2025",1,0)')

    def test_doubled_quote_escape_preserved(self):
        self.assertEqual(
            shift_formula('IF(A5="say ""Q1""",B5)', 1, 2),
            'IF(A7="say ""Q1""",B7)')


class ColumnLimitTest(unittest.TestCase):
    """Tokens beyond the XFD column limit are not references."""

    def test_four_letter_token_preserved(self):
        self.assertEqual(shift_formula("ABCD12+A5", 5, 1), "ABCD12+A6")
        self.assertEqual(shift_formula("ABCD12", 2, 1), "ABCD12")


class RealReferenceTest(unittest.TestCase):
    """Genuine A1-style references (the feature itself) must still shift."""

    def test_relative_range(self):
        self.assertEqual(shift_formula("SUM(B2:B10)", 2, 1), "SUM(B3:B11)")

    def test_absolute_and_mixed_anchors(self):
        self.assertEqual(shift_formula("$B$7", 5, 1), "$B$8")
        self.assertEqual(shift_formula("B$7", 5, 1), "B$8")
        self.assertEqual(shift_formula("$B7", 5, 1), "$B8")

    def test_rows_below_at_untouched(self):
        self.assertEqual(shift_formula("A1+B2", 3, 1), "A1+B2")

    def test_max_column_limit_still_shifts(self):
        # Pin the column cap with an in-range row (1048576 + 1 would be
        # past Excel's row limit, i.e. a ref Excel itself rejects).
        self.assertEqual(shift_formula("XFD100", 2, 1), "XFD101")

    def test_delete_delta(self):
        self.assertEqual(shift_formula("LOG10(B7)", 3, -1), "LOG10(B6)")


class EndToEndShiftTest(unittest.TestCase):
    """Drive the real insert path over an unpacked worksheet directory."""

    WORKSHEET = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<worksheet '
        'xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        '<sheetData>'
        '<row r="1">'
        '<c r="A1"><f>LOG10(B5)</f></c>'
        '<c r="B1"><f>B5*1E3</f></c>'
        '<c r="C1"><f>$B$5+B$6</f></c>'
        '<c r="D1"><f>Q1!B5</f></c>'
        '<c r="E1"><f>Q1!B5:B7</f></c>'
        '<c r="F1"><f>TBL1[#All]</f></c>'
        '<c r="G1"><f>Q1_TOTAL+A5</f></c>'
        '<c r="H1"><f>_xlfn.A1</f></c>'
        '<c r="I1"><f>IF(A5="Q3",B5,C5)</f></c>'
        '<c r="J1"><f>IF($A$5="FY2025",1,0)</f></c>'
        '<c r="K1"><f>ABCD12</f></c>'
        '</row>'
        '</sheetData>'
        '</worksheet>'
    )

    EXPECTED = [
        "LOG10(B7)",
        "B7*1E3",
        "$B$7+B$8",
        "Q1!B7",
        "Q1!B7:B9",
        "TBL1[#All]",
        "Q1_TOTAL+A7",
        "_xlfn.A1",
        'IF(A7="Q3",B7,C7)',
        'IF($A$7="FY2025",1,0)',
        "ABCD12",
    ]

    def test_insert_two_rows_at_top(self):
        with tempfile.TemporaryDirectory() as work_dir:
            ws_dir = os.path.join(work_dir, "xl", "worksheets")
            os.makedirs(ws_dir)
            sheet = os.path.join(ws_dir, "sheet1.xml")
            with open(sheet, "w", encoding="utf-8") as fh:
                fh.write(self.WORKSHEET)

            script = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")
            proc = subprocess.run(
                [sys.executable, script, work_dir, "insert", "1", "2"],
                capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)

            ns = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
            root = ET.parse(sheet).getroot()
            formulas = [f.text for f in root.iter(f"{ns}f")]
            self.assertEqual(formulas, self.EXPECTED)


class QuotedSheetEndToEndTest(unittest.TestCase):
    r"""Quoted sheet names through the real CLI, insert AND delete.

    Import-level pins alone missed the [^\]] regression that froze
    refs behind quoted sheet names, so these run the actual CLI over a
    worksheet whose formulas exercise every quoting shape: quoted sheet
    names (with '' escape), external workbook prefixes (path and index
    form), bracketed column names with ']' / '' escapes, and an
    apostrophe inside a double-quoted literal.
    """

    WORKSHEET = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<worksheet '
        'xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        '<sheetData>'
        '<row r="1">'
        '<c r="A1"><f>\'Sheet A\'!A5+\'Sheet B\'!B5</f></c>'
        '<c r="B1"><f>\'[Book.xlsx]Q1 Data\'!A5</f></c>'
        '<c r="C1"><f>\'[1]Q1\'!A5</f></c>'
        '<c r="D1"><f>\'S\'!A5+B5+"don\'t"</f></c>'
        '<c r="E1"><f> SUM(Sales[[Budget\']\']]])+C5+\'Sheet A\'!D5</f></c>'
        '<c r="F1"><f>Sales[[Total\']\'\'FY2025]]+B5</f></c>'
        '<c r="G1"><f>TBL1[[A1\']\'\'A2]]+B5</f></c>'
        '<c r="H1"><f>\'It\'\'s a sheet\'!A5</f></c>'
        '</row>'
        '</sheetData>'
        '</worksheet>'
    )

    EXPECTED_INSERT_1_2 = [
        "'Sheet A'!A7+'Sheet B'!B7",
        "'[Book.xlsx]Q1 Data'!A7",
        "'[1]Q1'!A7",
        "'S'!A7+B7+\"don't\"",
        " SUM(Sales[[Budget']']]])+C7+'Sheet A'!D7",
        "Sales[[Total']''FY2025]]+B7",
        "TBL1[[A1']''A2]]+B7",
        "'It''s a sheet'!A7",
    ]

    EXPECTED_DELETE_3_1 = [
        "'Sheet A'!A4+'Sheet B'!B4",
        "'[Book.xlsx]Q1 Data'!A4",
        "'[1]Q1'!A4",
        "'S'!A4+B4+\"don't\"",
        " SUM(Sales[[Budget']']]])+C4+'Sheet A'!D4",
        "Sales[[Total']''FY2025]]+B4",
        "TBL1[[A1']''A2]]+B4",
        "'It''s a sheet'!A4",
    ]

    def _run_cli(self, work_dir):
        script = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")
        proc = subprocess.run(
            [sys.executable, script, work_dir, self.operation,
             str(self.at), str(self.count)],
            capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        ws_dir = os.path.join(work_dir, "xl", "worksheets")
        sheet = os.path.join(ws_dir, "sheet1.xml")
        ns = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
        root = ET.parse(sheet).getroot()
        return [f.text for f in root.iter(f"{ns}f")]

    def _write_worksheet(self, work_dir):
        ws_dir = os.path.join(work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        with open(os.path.join(ws_dir, "sheet1.xml"), "w", encoding="utf-8") as fh:
            fh.write(self.WORKSHEET)

    def test_insert_two_rows_at_top(self):
        self.operation, self.at, self.count = "insert", 1, 2
        with tempfile.TemporaryDirectory() as work_dir:
            self._write_worksheet(work_dir)
            self.assertEqual(self._run_cli(work_dir), self.EXPECTED_INSERT_1_2)

    def test_delete_one_row_at_three(self):
        self.operation, self.at, self.count = "delete", 3, 1
        with tempfile.TemporaryDirectory() as work_dir:
            self._write_worksheet(work_dir)
            self.assertEqual(self._run_cli(work_dir), self.EXPECTED_DELETE_3_1)


if __name__ == "__main__":
    unittest.main()
