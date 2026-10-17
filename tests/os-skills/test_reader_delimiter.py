"""Discover actual delimited exports, including quoting and literal separators."""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "src/os-skills/others/xlsx/scripts/xlsx_reader.py"


class DelimiterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("reader_delimiter", SCRIPT)
        cls.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.reader)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def read(self, path, *options):
        output, errors = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(sys, "argv", [str(SCRIPT), str(path), "--json", *options]),
            contextlib.redirect_stdout(output),
            contextlib.redirect_stderr(errors),
        ):
            try:
                self.reader.main()
            except SystemExit as error:
                return error.code, output.getvalue(), errors.getvalue()
        return 0, output.getvalue(), errors.getvalue()

    def test_semicolon_export_retains_columns_and_numeric_values(self):
        path = self.root / "regional.csv"
        path.write_text("Product;Amount\nAlpha;7\nBeta;3\n", encoding="utf-8")
        code, output, errors = self.read(path, "--delimiter", ";")
        self.assertEqual(code, 0, errors)
        report = json.loads(output)
        structure = report["structure"]["regional"]
        self.assertEqual(structure["shape"], {"rows": 2, "cols": 2})
        self.assertEqual(structure["columns"], ["Product", "Amount"])
        self.assertEqual(
            structure["preview"],
            [{"Product": "Alpha", "Amount": 7}, {"Product": "Beta", "Amount": 3}],
        )
        self.assertEqual(path.read_text(encoding="utf-8"), "Product;Amount\nAlpha;7\nBeta;3\n")

    def test_csv_quoting_protects_delimiters_inside_cell_text(self):
        path = self.root / "quoted.csv"
        path.write_text('Product;Amount\n"Alpha;Beta";7\n', encoding="utf-8")
        code, output, errors = self.read(path, "--delimiter", ";")
        self.assertEqual(code, 0, errors)
        self.assertEqual(
            json.loads(output)["structure"]["quoted"]["preview"],
            [{"Product": "Alpha;Beta", "Amount": 7}],
        )

    def test_regex_metacharacter_is_a_literal_one_character_separator(self):
        path = self.root / "pipe.csv"
        path.write_text("Product|Amount\nAlpha|7\n", encoding="utf-8")
        code, output, errors = self.read(path, "--delimiter", "|")
        self.assertEqual(code, 0, errors)
        self.assertEqual(json.loads(output)["structure"]["pipe"]["columns"], ["Product", "Amount"])

    def test_explicit_delimiter_overrides_tsv_suffix(self):
        path = self.root / "export.tsv"
        path.write_text("Product;Amount\nAlpha;7\n", encoding="utf-8")
        code, output, errors = self.read(path, "--delimiter", ";")
        self.assertEqual(code, 0, errors)
        self.assertEqual(json.loads(output)["structure"]["export"]["shape"]["cols"], 2)

    def test_omitted_delimiter_retains_comma_and_tab_defaults(self):
        for extension, delimiter in (("csv", ","), ("tsv", "\t")):
            with self.subTest(extension=extension):
                path = self.root / f"default.{extension}"
                path.write_text(f"Product{delimiter}Amount\nAlpha{delimiter}7\n", encoding="utf-8")
                code, output, errors = self.read(path)
                self.assertEqual(code, 0, errors)
                self.assertEqual(
                    json.loads(output)["structure"]["default"]["columns"], ["Product", "Amount"]
                )

    def test_invalid_delimiter_has_an_actionable_parser_error(self):
        path = self.root / "empty.csv"
        path.write_text("Product,Amount\n", encoding="utf-8")
        for delimiter in ("", "||", "\n", "\r", "\0"):
            with self.subTest(delimiter=repr(delimiter)):
                code, output, errors = self.read(path, "--delimiter", delimiter)
                self.assertEqual(code, 2)
                self.assertIn("single", errors.lower())
                self.assertIn("--delimiter", errors)
                self.assertNotIn("Traceback", errors)
                self.assertEqual(output, "")

    def test_excel_input_rejects_inapplicable_delimiter_before_loading(self):
        path = self.root / "workbook.xlsx"
        path.write_bytes(b"not a workbook")
        code, output, errors = self.read(path, "--delimiter", ";")
        self.assertEqual(code, 2)
        self.assertIn("CSV", errors)
        self.assertEqual(output, "")


if __name__ == "__main__":
    unittest.main()
