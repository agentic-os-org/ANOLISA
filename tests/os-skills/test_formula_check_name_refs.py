#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for formula_check.py named-range candidate extraction.

Regression tests for sheet-name false positives: the cleanup patterns
only stripped sheet prefixes followed by an uppercase A1-style
reference, so a 3D range (Sheet1:Sheet2!A1) left "Sheet1" behind and a
sheet-qualified name (Sheet1!myrate) left both "Sheet1" and "myrate"
behind — each then reported as unknown_name_ref against names that
exist (as sheets or as sheet-scoped names).
"""

import os
import sys
import tempfile
import time
import unittest
import zipfile
from pathlib import Path

SCRIPTS = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
)
sys.path.insert(0, str(SCRIPTS))
import formula_check as fc  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_OD_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"


def build_workbook(path, sheet_names, formulas, per_sheet=None):
    """Minimal xlsx: the formulas live on the first sheet, one per column.

    per_sheet: {sheet_name: [(cell_ref, formula), ...]} overrides the
    default "everything on the first sheet" layout, so cross-sheet
    scenarios (same cell address on different sheets) can be built.
    """
    sheets = "".join(
        f'<sheet name="{name}" sheetId="{i + 1}" r:id="rId{i + 1}"/>'
        for i, name in enumerate(sheet_names)
    )
    workbook = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_OD_REL}">'
        f"<sheets>{sheets}</sheets></workbook>"
    )
    rels = "".join(
        f'<Relationship Id="rId{i + 1}"'
        f' Type="{NS_OD_REL}/worksheet"'
        f' Target="worksheets/sheet{i + 1}.xml"/>'
        for i in range(len(sheet_names))
    )
    relationships = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<Relationships xmlns="{NS_PKG_REL}">{rels}</Relationships>'
    )

    def sheet_xml(entries):
        if not entries:
            return (
                '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
                f'<worksheet xmlns="{NS_MAIN}"><sheetData/></worksheet>'
            )
        cells = "".join(
            f'<c r="{cell_ref}"><f>{formula}</f></c>' for cell_ref, formula in entries
        )
        return (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<worksheet xmlns="{NS_MAIN}">'
            f"<sheetData><row r=\"1\">{cells}</row></sheetData></worksheet>"
        )

    if per_sheet is None:
        per_sheet = {
            sheet_names[0]: [
                (f"{chr(ord('A') + i)}1", formula) for i, formula in enumerate(formulas)
            ]
        }
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("xl/workbook.xml", workbook)
        z.writestr("xl/_rels/workbook.xml.rels", relationships)
        for i, name in enumerate(sheet_names):
            z.writestr(
                f"xl/worksheets/sheet{i + 1}.xml", sheet_xml(per_sheet.get(name, []))
            )


class TestExtractNameRefs(unittest.TestCase):
    def test_3d_range_leaves_no_candidates(self):
        """SUM(Sheet1:Sheet2!A1) references sheets, not names.

        Before the fix the strip removed only "Sheet2!A1", leaving
        "Sheet1" as a candidate and a false unknown_name_ref warning.
        """
        self.assertEqual(fc.extract_name_refs("=SUM(Sheet1:Sheet2!A1)"), [])

    def test_sheet_qualified_name_leaves_no_candidates(self):
        """Sheet1!myrate is a sheet-scoped reference, not a workbook name."""
        self.assertEqual(fc.extract_name_refs("=Sheet1!myrate*2"), [])

    def test_bare_name_still_extracted(self):
        """Control: an unqualified name is still a candidate."""
        self.assertEqual(fc.extract_name_refs("=myrate*2"), ["myrate"])

    def test_quoted_sheet_ref_still_stripped(self):
        # the escaped-apostrophe form must strip fully: a >=3 char left
        # fragment (Mary) would surface as a false candidate otherwise
        self.assertEqual(
            fc.extract_name_refs("='Q1 Data'!A1+'Mary''s Sheet'!B2"), []
        )

    def test_escaped_apostrophe_left_fragment_not_candidate(self):
        self.assertEqual(
            fc.extract_name_refs("=SUM('John''s Data'!B2)"), []
        )

    def test_trailing_escaped_apostrophe_not_candidate(self):
        self.assertEqual(fc.extract_name_refs("='Mary'''!B2"), [])

    def test_escaped_apostrophe_mid_name_keeps_real_name(self):
        self.assertEqual(
            fc.extract_name_refs("=SUM('A''B'!A1:B9)+Total"), ["Total"]
        )

    def test_mixed_formula_keeps_real_name(self):
        """A real name next to a 3D range is still reported."""
        self.assertEqual(
            fc.extract_name_refs("=TOTALS*SUM(Sheet1:Sheet2!A1)"), ["TOTALS"]
        )

    def test_sheet_ref_plus_real_name_coexist(self):
        """A real name next to a plain sheet-qualified range is still reported."""
        self.assertEqual(
            fc.extract_name_refs("=SUM(Sheet1!A1:B9)+Revenue"), ["Revenue"]
        )

    def test_3d_quoted_alternatives_leave_no_candidates(self):
        """Both quoted alternatives of the 3D pattern must strip fully."""
        self.assertEqual(
            fc.extract_name_refs("=SUM('Q1 Data':'Q2 Data'!A1)"), []
        )
        self.assertEqual(
            fc.extract_name_refs("=SUM('Q1 Data':Sheet2!A1)"), []
        )

    def test_colon_joined_sheet_refs_leave_no_candidates(self):
        """X!ref:Y!ref must not feed the 3D pattern from after the first "!".

        Without the (?<!!) guards the 3D pattern eats "A1:Sheet1" out of
        Sheet1!A1:Sheet1!B2 and leaves an unstrippable "Sheet1!" tail,
        reporting the sheet itself as an unknown name.
        """
        self.assertEqual(
            fc.extract_name_refs("=Sheet1!A1:Sheet1!B2"), []
        )
        self.assertEqual(
            fc.extract_name_refs("=SUM('Q1 Data'!A1:'Mary''s Sheet'!B2)"), []
        )

    def test_colon_joined_sheet_refs_keep_real_name(self):
        self.assertEqual(
            fc.extract_name_refs("=SUM(Data!A1:Data!B2)+Rate"), ["Rate"]
        )
        self.assertEqual(
            fc.extract_name_refs("=SUM('Mary''s Sheet'!A1:Data!B2)+Rate"),
            ["Rate"],
        )
        self.assertEqual(
            fc.extract_name_refs("=SUM('Sheet 1'!A1:Sheet1!B2)+Rate"),
            ["Rate"],
        )

    def test_cjk_sheet_prefix_leaves_no_candidates(self):
        """The plain token matches extract_sheet_refs', CJK included."""
        self.assertEqual(fc.extract_name_refs("=销售!myrate*2"), [])

    def test_external_workbook_prefix_not_candidates(self):
        """'[Book.xlsx]Data'!A1 is an external reference, not four names.

        The 31-char bound applies to the sheet name; the quoted token may
        also carry an external workbook prefix that must strip too — the
        short bracket form, and the full path form where the bracket sits
        at the end of the path.
        """
        self.assertEqual(
            fc.extract_name_refs("='[Financial Budget 2026.xlsx]Data'!A1"), []
        )
        self.assertEqual(
            fc.extract_name_refs("='[Financial Budget 2026.xlsx]Data'!A1+Revenue"),
            ["Revenue"],
        )
        self.assertEqual(
            fc.extract_name_refs("=[1]Prices!A1+Revenue"), ["Revenue"]
        )
        self.assertEqual(
            fc.extract_name_refs(r"='C:\Dir\[Book.xlsx]Data'!A1+Revenue"),
            ["Revenue"],
        )
        self.assertEqual(
            fc.extract_name_refs(r"='/home/models/[FY26 Model.xlsx]Assumptions'!B12"),
            [],
        )
        self.assertEqual(
            fc.extract_name_refs(
                r"='C:\A very long path name indeed"
                r"\[Financial Model FY2026 Final v3.xlsx]Assumptions'!A1"
            ),
            [],
        )

    def test_long_path_external_prefix_beyond_max_path_still_stripped(self):
        """Windows MAX_PATH is 260, so a legal external prefix can exceed
        219 characters — OneDrive/SharePoint paths reach 100+ on their own.
        No length cliff may exist in the prefix.
        """
        for depth in (220, 262, 400):
            pad = "d" * (depth - 14)  # "C:\" + pad + "[Book.xlsx]" == depth
            formula = "='C:\\" + pad + "[Book.xlsx]Sheet1'!A1+Revenue"
            self.assertEqual(
                fc.extract_name_refs(formula), ["Revenue"], f"prefix={depth}"
            )

    def test_31_char_quoted_sheet_name_still_stripped(self):
        """The bounded quantifier must still admit Excel's longest sheet name."""
        sheet = "'" + "a" * 31 + "'"
        self.assertEqual(
            fc.extract_name_refs(f"={sheet}!A1+Revenue"), ["Revenue"]
        )

    def test_unclosed_apostrophe_run_stays_fast(self):
        """An unclosed apostrophe run must not trigger quadratic backtracking.

        The unbounded escape-aware quantifier took ~2.2s on an
        8192-character payload (Excel's formula length cap); the bounded
        form stays in the tens of milliseconds.
        """
        payload = "=SUM('" + "'" * 8192
        start = time.perf_counter()
        fc.extract_name_refs(payload)
        self.assertLess(time.perf_counter() - start, 0.5)


class TestExtract3DStartSheets(unittest.TestCase):
    def test_plain_and_quoted_endpoints(self):
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM(Sheet1:Sheet2!A1)"), ["Sheet1"]
        )
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM('Q1 Data':'Q2 Data'!A1)"),
            ["Q1 Data"],
        )
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM('John''s Data':Sheet2!A1)"),
            ["John's Data"],
        )

    def test_external_ranges_skipped(self):
        # both endpoints quoted as one token
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM('[Book.xlsx]S1:S2'!A1)"), []
        )
        # only the first endpoint carries the workbook prefix
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM('[Book.xlsx]S1':S2!A1)"), []
        )
        # full path form: Excel forbids "[" / "]" in sheet names, so a
        # quoted endpoint containing "]" always points at another workbook
        self.assertEqual(
            fc.extract_3d_start_sheets(r"=SUM('C:\Docs\[Book.xlsx]Sheet9':Sheet2!A1)"),
            [],
        )

    def test_cell_ref_first_endpoint_is_not_a_sheet(self):
        """SUM(A1:Sheet1!A1) has a local range on the left of the colon.

        A cell reference matches the plain sheet token but can never be a
        sheet name; reporting it as broken_sheet_ref would flip the exit
        code on a perfectly legal formula.
        """
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM(A1:Sheet1!A1)"), []
        )
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM(B2:Sheet1!C9)"), []
        )
        # a real missing sheet is still reported
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM(MissingSheet:Sheet1!A1)"),
            ["MissingSheet"],
        )

    def test_colon_joined_refs_are_not_3d(self):
        self.assertEqual(
            fc.extract_3d_start_sheets("=SUM(Sheet1!A1:Sheet1!B2)"), []
        )


class TestCheck3DStartSheet(unittest.TestCase):
    """check()-level regression: 3D first endpoints are sheet references."""

    def check_formulas(self, sheet_names, formulas):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "workbook.xlsx")
            build_workbook(path, sheet_names, formulas)
            return fc.check(path)

    def check_multi_sheet(self, sheet_names, per_sheet):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "workbook.xlsx")
            build_workbook(path, sheet_names, [], per_sheet=per_sheet)
            return fc.check(path)

    def test_missing_3d_first_endpoint_reported(self):
        results = self.check_formulas(
            ["Sheet1"], ["SUM(MissingSheet:Sheet1!A1)"]
        )
        broken = [
            e["missing_sheet"]
            for e in results["errors"]
            if e["type"] == "broken_sheet_ref"
        ]
        self.assertEqual(broken, ["MissingSheet"])
        # the record must carry the same heuristic note as Check 3, so
        # downstream consumers can tell heuristic output from certain one
        rec = next(
            e for e in results["errors"] if e["type"] == "broken_sheet_ref"
        )
        self.assertIn("Heuristic", rec["note"])

    def test_same_missing_sheet_reported_once_per_cell(self):
        """Missing:Missing!A1 is one defect, not two identical records.

        Check 2 already reports the second endpoint; Check 2b must not
        repeat it. Two distinct missing endpoints still report both.
        """
        results = self.check_formulas(
            ["Sheet1"], ["SUM(MissingSheet:MissingSheet!A1)"]
        )
        broken = [
            e["missing_sheet"]
            for e in results["errors"]
            if e["type"] == "broken_sheet_ref"
        ]
        self.assertEqual(broken, ["MissingSheet"])

        results = self.check_formulas(
            ["Sheet1"], ["SUM(Left:Right!A1)"]
        )
        broken = [
            e["missing_sheet"]
            for e in results["errors"]
            if e["type"] == "broken_sheet_ref"
        ]
        # Check 2 reports the second endpoint first; order is not the contract
        self.assertEqual(sorted(broken), ["Left", "Right"])

    def test_dedup_does_not_cross_sheets(self):
        """Sheet1!A1 and Sheet2!A1 are different cells: a report on one
        must not suppress the other.
        """
        results = self.check_multi_sheet(
            ["Sheet1", "Sheet2", "Data"],
            {
                "Sheet1": [("A1", "Missing!B1")],
                "Sheet2": [("A1", "SUM(Missing:Data!A1)")],
            },
        )
        broken = sorted(
            (e["sheet"], e["missing_sheet"])
            for e in results["errors"]
            if e["type"] == "broken_sheet_ref"
        )
        self.assertEqual(broken, [("Sheet1", "Missing"), ("Sheet2", "Missing")])

    def test_valid_3d_range_is_clean(self):
        results = self.check_formulas(
            ["Sheet1", "Sheet2"], ["SUM(Sheet1:Sheet2!A1)"]
        )
        self.assertEqual(results["error_count"], 0)

    def test_local_range_left_of_sheet_ref_keeps_exit_code_clean(self):
        """SUM(A1:Sheet1!A1) is legal: the colon's left side is a local
        cell range, not a missing sheet — the exit code must stay 0.
        """
        results = self.check_formulas(
            ["Sheet1"], ["SUM(A1:Sheet1!A1)"]
        )
        self.assertEqual(results["error_count"], 0)
        self.assertEqual(
            [e["type"] for e in results["errors"] if e["cell"] != "?"], []
        )


if __name__ == "__main__":
    unittest.main()
