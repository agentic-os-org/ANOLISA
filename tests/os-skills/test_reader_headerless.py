"""Keep the first record when analyzing files with no header row."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from openpyxl import Workbook

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_reader.py"


class HeaderlessReaderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("headerless_reader", SCRIPT)
        cls.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.reader)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def cli(self, path, *flags):
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(path), "--json", *flags],
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
            env={**os.environ, "PYTHONUTF8": "1"},
        )

    def test_csv_retains_first_record_and_statistics(self):
        path = self.root / "records.csv"
        path.write_text("1,10\n2,20\n3,30\n", encoding="utf-8")
        original = path.read_bytes()
        result = self.cli(path, "--no-header")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        sheet = report["structure"]["records"]
        self.assertEqual(sheet["columns"], ["column_1", "column_2"])
        self.assertEqual(sheet["shape"], {"rows": 3, "cols": 2})
        self.assertEqual(sheet["preview"][0], {"column_1": 1, "column_2": 10})
        self.assertEqual(report["stats"]["records"]["column_2"]["mean"], 20)
        self.assertEqual(path.read_bytes(), original)

    def test_tsv_and_quoted_csv_keep_first_text_record(self):
        for name, text in (
            ("pairs.tsv", "first\t10\nsecond\t20\n"),
            ("pairs.csv", '"first, label",10\n"second, label",20\n'),
        ):
            with self.subTest(name=name):
                path = self.root / name
                path.write_text(text, encoding="utf-8")
                sheets = self.reader.detect_and_load(str(path), no_header=True)
                frame = sheets["pairs"]
                self.assertEqual(len(frame), 2)
                self.assertEqual(list(frame.columns), ["column_1", "column_2"])
                self.assertTrue(frame.iloc[0, 0].startswith("first"))

    def workbook(self):
        path = self.root / "records.xlsx"
        wb = Workbook()
        for name, values in (("First", [(1, 10), (2, 20)]), ("Second", [(3, 30), (4, 40)])):
            ws = wb.active if name == "First" else wb.create_sheet()
            ws.title = name
            for row in values:
                ws.append(row)
        wb.save(path)
        wb.close()
        return path

    def test_all_excel_sheets_get_stable_generated_columns(self):
        path = self.workbook()
        original = path.read_bytes()
        sheets = self.reader.detect_and_load(str(path), no_header=True)
        self.assertEqual(list(sheets), ["First", "Second"])
        for frame in sheets.values():
            self.assertEqual(frame.shape, (2, 2))
            self.assertEqual(list(frame.columns), ["column_1", "column_2"])
        self.assertEqual(sheets["Second"].iloc[0, 0], 3)
        self.assertEqual(path.read_bytes(), original)

    def test_selected_excel_sheet_retains_first_row_in_quality_mode(self):
        result = self.cli(self.workbook(), "--no-header", "--sheet", "Second", "--quality")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(list(report["structure"]), ["Second"])
        self.assertEqual(report["structure"]["Second"]["preview"][0]["column_1"], 3)
        self.assertEqual(report["stats"], {})

    def test_single_record_and_missing_values(self):
        path = self.root / "one.csv"
        path.write_text("first,,3\n", encoding="utf-8")
        result = self.cli(path, "--no-header")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report["structure"]["one"]["shape"], {"rows": 1, "cols": 3})
        self.assertEqual(report["quality"]["one"][0]["column"], "column_2")

    def test_default_csv_header_semantics_are_unchanged(self):
        path = self.root / "named.csv"
        path.write_text("Name,Amount\nfirst,10\n", encoding="utf-8")
        frame = self.reader.detect_and_load(str(path))["named"]
        self.assertEqual(list(frame.columns), ["Name", "Amount"])
        self.assertEqual(frame.shape, (1, 2))

    def test_default_excel_header_semantics_are_unchanged(self):
        frame = self.reader.detect_and_load(str(self.workbook()), "First")["First"]
        self.assertEqual(list(frame.columns), [1, 10])
        self.assertEqual(frame.shape, (1, 2))


if __name__ == "__main__":
    unittest.main()
