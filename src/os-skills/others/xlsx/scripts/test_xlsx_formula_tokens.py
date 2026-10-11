#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Check token preservation through the real row-shift helpers and XML CLI."""

from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET


SCRIPT = Path(__file__).with_name("xlsx_shift_rows.py")
MODULE = runpy.run_path(str(SCRIPT), run_name="xlsx_shift_rows_tests")
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
FORMULAS = [
    "LOG10(A1)",
    "1E5+A5",
    'IF(A7="B7",A7,0)',
    "SHEET5!A5",
    "SUM(A5:A7)",
]


def write_worksheet(path):
    root = ET.Element(f"{{{NS}}}worksheet")
    ET.SubElement(root, f"{{{NS}}}dimension", ref="A1:E7")
    data = ET.SubElement(root, f"{{{NS}}}sheetData")
    row = ET.SubElement(data, f"{{{NS}}}row", r="7")
    for col, formula in zip("ABCDE", FORMULAS):
        cell = ET.SubElement(row, f"{{{NS}}}c", r=f"{col}7")
        ET.SubElement(cell, f"{{{NS}}}f").text = formula
    ET.ElementTree(root).write(path, encoding="utf-8")


class FormulaTokenTests(unittest.TestCase):
    def assert_shift(self, formula, expected, at=5, delta=1):
        self.assertEqual(MODULE["shift_formula"](formula, at, delta), expected)

    def test_preserves_builtin_function_names(self):
        self.assert_shift("LOG10(A5)+ATAN2(A5,B7)", "LOG10(A6)+ATAN2(A6,B8)")
        self.assert_shift("IMLOG2(A5)+DAYS360(A5,B7)", "IMLOG2(A6)+DAYS360(A6,B8)")

    def test_preserves_scientific_constants(self):
        self.assert_shift("1E5+2.5E5+1E+5+1E-5+A5", "1E5+2.5E5+1E+5+1E-5+A6")

    def test_preserves_strings_and_escaped_quotes(self):
        self.assert_shift(
            'IF(A5="Bob\'s A5",A5,"He said ""B5""")',
            'IF(A6="Bob\'s A5",A6,"He said ""B5""")',
        )
        self.assert_shift('"SHEET5!A5"&""&A5', '"SHEET5!A5"&""&A6')

    def test_preserves_sheet_qualifiers(self):
        self.assert_shift("SHEET5!A5+SHEET5:SHEET7!B7", "SHEET5!A6+SHEET5:SHEET7!B8")
        self.assert_shift("'BOB''S FY2025'!$A$5", "'BOB''S FY2025'!$A$6")

    def test_preserves_identifier_boundaries(self):
        self.assert_shift("RATE5+A5_TOTAL+TOTAL.A5+A5", "RATE5+A5_TOTAL+TOTAL.A5+A6")
        self.assert_shift("XFE5+A1048577+XFD5", "XFE5+A1048577+XFD6")
        self.assert_shift("\u6570\u636eA5+A5", "\u6570\u636eA5+A6")

    def test_preserves_bracketed_structured_tokens(self):
        self.assert_shift("TABLE5[[#Headers],[A5]]+A5", "TABLE5[[#Headers],[A5]]+A6")

    def test_shifts_actual_absolute_mixed_and_range_references(self):
        self.assert_shift(
            "SUM(A1:A5)+$A$5+$A5+A$5+LOG10",
            "SUM(A1:A6)+$A$6+$A6+A$6+LOG11",
        )

    def test_keeps_cell_calls_and_intersections_as_references(self):
        self.assert_shift("A5(A7)+LOG10(A5)", "A6(A8)+LOG10(A6)")
        self.assert_shift("A5 (B7)+LOG10 (A5)", "A6 (B8)+LOG11 (A6)")

    def test_preserves_existing_sqref_and_chart_range_shifts(self):
        self.assertEqual(MODULE["shift_sqref"]("A1:A5 B7", 5, 1), "A1:A6 B8")
        self.assertEqual(
            MODULE["shift_chart_range"]("'FY2025'!$A$5:$A$7", 5, 1),
            "'FY2025'!$A$6:$A$8",
        )

    def assert_worksheet(self, path, delta):
        root = ET.parse(path).getroot()
        self.assertEqual(
            [cell.get("r") for cell in root.findall(f".//{{{NS}}}c")],
            [f"{col}{7 + delta}" for col in "ABCDE"],
        )
        self.assertEqual(
            [node.text for node in root.findall(f".//{{{NS}}}f")],
            [
                "LOG10(A1)",
                f"1E5+A{5 + delta}",
                f'IF(A{7 + delta}="B7",A{7 + delta},0)',
                f"SHEET5!A{5 + delta}",
                f"SUM(A{5 + delta}:A{7 + delta})",
            ],
        )
        self.assertEqual(root.find(f"{{{NS}}}dimension").get("ref"), f"A1:E{7 + delta}")

    def test_actual_xml_processor_preserves_tokens_for_insert_and_delete(self):
        for delta in (1, -1):
            with self.subTest(delta=delta), tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "sheet.xml"
                write_worksheet(path)
                MODULE["process_worksheet"](str(path), 5, delta)
                self.assert_worksheet(path, delta)

    def test_real_cli_updates_xml_references_without_changing_tokens(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            worksheet = root / "xl/worksheets/sheet1.xml"
            worksheet.parent.mkdir(parents=True)
            write_worksheet(worksheet)
            table = root / "xl/tables/table1.xml"
            table.parent.mkdir(parents=True)
            ET.ElementTree(ET.Element(f"{{{NS}}}table", ref="A5:E7")).write(table)

            result = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), str(root), "insert", "5", "1"],
                capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assert_worksheet(worksheet, 1)
            self.assertEqual(ET.parse(table).getroot().get("ref"), "A6:E8")


if __name__ == "__main__":
    unittest.main()
