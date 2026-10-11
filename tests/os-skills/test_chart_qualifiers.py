"""Keep chart source qualifiers intact while moving local data ranges."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_shift_rows.py"
)


class ChartQualifierTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("chart_shift", SCRIPT)
        cls.shift = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.shift)

    def test_quoted_bang_and_digit_sheet_names_remain_intact(self):
        for name in ("Revenue!FY2025", "O''Brien!FY2025", "FY2025!Revenue"):
            with self.subTest(name=name):
                reference = f"'{name}'!$A$5:$B$10"
                self.assertEqual(
                    self.shift.shift_chart_range(reference, 5, 2), f"'{name}'!$A$7:$B$12"
                )

    def test_external_workbook_ranges_remain_unchanged(self):
        for reference in (
            "'[budget.xlsx]Revenue'!$A$5:$B$10",
            "[budget.xlsx]Sales!A5:A10",
            "'C:\\exports\\[budget.xlsx]Revenue!FY2025'!$A$5",
        ):
            with self.subTest(reference=reference):
                self.assertEqual(self.shift.shift_chart_range(reference, 5, 2), reference)

    def test_local_unquoted_and_quoted_ranges_still_move(self):
        for reference, expected in (
            ("Sheet1!$A$5:$A$10", "Sheet1!$A$7:$A$12"),
            ("'Q1 Data'!B4:B10", "'Q1 Data'!B4:B12"),
            ("'O''Brien'!A5", "'O''Brien'!A7"),
        ):
            with self.subTest(reference=reference):
                self.assertEqual(self.shift.shift_chart_range(reference, 5, 2), expected)

    def test_delete_keeps_sheet_name_and_moves_local_rows(self):
        self.assertEqual(
            self.shift.shift_chart_range("'Revenue!FY2025'!$A$8:$A$12", 8, -2),
            "'Revenue!FY2025'!$A$6:$A$10",
        )

    def test_unknown_and_three_dimensional_sources_are_not_rewritten(self):
        for reference in ("A5:A10", "NamedSeries", "'Jan:Mar'!A5:A10", "Jan:Mar!A5:A10"):
            with self.subTest(reference=reference):
                self.assertEqual(self.shift.shift_chart_range(reference, 5, 2), reference)

    def test_actual_chart_xml_updates_only_local_range(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "chart.xml"
            path.write_text(
                '<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">'
                "<c:f>'Revenue!FY2025'!$A$5:$A$10</c:f>"
                "<c:f>'[external.xlsx]Data'!$A$5:$A$10</c:f>"
                "</c:chartSpace>",
                encoding="utf-8",
            )
            self.assertEqual(self.shift.process_chart(str(path), 5, 2), 1)
            result = path.read_text(encoding="utf-8")
            self.assertIn("'Revenue!FY2025'!$A$7:$A$12", result)
            self.assertIn("'[external.xlsx]Data'!$A$5:$A$10", result)


if __name__ == "__main__":
    unittest.main()
