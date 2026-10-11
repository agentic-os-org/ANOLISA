"""Entire-row formula ranges must follow worksheet row edits."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py"
)
SPEC = importlib.util.spec_from_file_location("whole_row_test_shifter", SCRIPT)
SHIFTER = importlib.util.module_from_spec(SPEC)
with mock.patch.object(sys, "path", [str(SCRIPT.parent), *sys.path]):
    SPEC.loader.exec_module(SHIFTER)

NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
CHART_NS = "http://schemas.openxmlformats.org/drawingml/2006/chart"


class WholeRowRangeTests(unittest.TestCase):
    def test_insert_preserves_absolute_markers_and_shifts_each_endpoint(self):
        examples = {
            "SUM(5:10)": "SUM(7:12)",
            "SUM($5:$10)": "SUM($7:$12)",
            "SUM($5:10,5:$10)": "SUM($7:12,7:$12)",
            "SUM(2:10)+A7+$B$7+B:B": "SUM(2:12)+A9+$B$9+B:B",
            "SUM(2:4)": "SUM(2:4)",
            "SUM(5:10,20:30)": "SUM(7:12,22:32)",
            "SUM(5:10!A7)": "SUM(5:10!A9)",
        }
        for before, after in examples.items():
            with self.subTest(formula=before):
                self.assertEqual(SHIFTER.shift_formula(before, 5, 2), after)

    def test_delete_uses_the_existing_row_one_boundary_and_worksheet_limit(self):
        self.assertEqual(SHIFTER.shift_formula("SUM(2:$10)", 2, -3), "SUM(1:$7)")
        self.assertEqual(SHIFTER.shift_formula("SUM(5:10)", 5, -2), "SUM(3:8)")
        self.assertEqual(
            SHIFTER.shift_formula("SUM(1048575:1048576)", 1048575, 2),
            "SUM(1048576:1048576)",
        )

    def test_sheet_workbook_and_three_dimensional_qualifiers_are_preserved(self):
        for qualifier in (
            "Sheet1!",
            "'FY2025 data'!",
            "'O''Brien'!",
            "Sheet1:Sheet3!",
            "'[Book1.xlsx]FY2025'!",
            "[Book1.xlsx]Sheet1!",
        ):
            with self.subTest(qualifier=qualifier):
                self.assertEqual(
                    SHIFTER.shift_formula(f"SUM({qualifier}$5:10)", 5, 2),
                    f"SUM({qualifier}$7:12)",
                )

    def test_string_literals_and_structured_labels_do_not_become_row_references(self):
        examples = (
            'IF(A7>0,"5:10",SUM(5:10))',
            "IF(A7>0,\"a'b'5:10\",SUM(5:10))",
            'IF(A7>0,"quoted ""5:10""",SUM(5:10))',
            "SUM(Table[5:10],5:10)",
            "SUM(Table[[#Headers],[5:10]],5:10)",
        )
        for formula in examples:
            with self.subTest(formula=formula):
                expected = formula.replace("A7", "A9")
                expected = expected.rsplit("5:10", 1)
                self.assertEqual(SHIFTER.shift_formula(formula, 5, 2), "7:12".join(expected))

    def test_numeric_fragments_and_invalid_ranges_are_unchanged(self):
        for formula in ("1.5:10", "5:10.5", "named5:10", "5:10name", "$0:$10", "1:1048577"):
            with self.subTest(formula=formula):
                self.assertEqual(SHIFTER.shift_formula(formula, 5, 2), formula)

    def test_cli_updates_worksheet_and_chart_formula_sources(self):
        for operation, expected in (("insert", "7:12"), ("delete", "3:8")):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                (work / "xl/worksheets").mkdir(parents=True)
                (work / "xl/charts").mkdir()
                worksheet = work / "xl/worksheets/sheet1.xml"
                chart = work / "xl/charts/chart1.xml"
                worksheet.write_text(
                    f'<worksheet xmlns="{NS}"><sheetData><row r="2"><c r="A2">'
                    "<f>SUM(5:10)</f></c></row></sheetData></worksheet>",
                    encoding="utf-8",
                )
                chart.write_text(
                    f'<chartSpace xmlns="{CHART_NS}"><chart><numRef>'
                    "<f>'FY2025 data'!5:10</f></numRef></chart></chartSpace>",
                    encoding="utf-8",
                )
                result = subprocess.run(
                    [sys.executable, str(SCRIPT), str(work), operation, "5", "2"],
                    capture_output=True,
                    text=True,
                    timeout=20,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(ET.parse(worksheet).find(f".//{{{NS}}}f").text, f"SUM({expected})")
                self.assertEqual(
                    ET.parse(chart).find(f".//{{{CHART_NS}}}f").text,
                    f"'FY2025 data'!{expected}",
                )


if __name__ == "__main__":
    unittest.main()
