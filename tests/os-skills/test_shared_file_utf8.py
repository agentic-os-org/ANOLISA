"""The UTF-8 file marker is not part of the first spreadsheet value."""

import contextlib
import importlib.util
import io
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest import mock

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/xlsx/scripts/shared_strings_builder.py"
)


class SharedFileUtf8Tests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("shared_file", SCRIPT)
        cls.builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.builder)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "strings.txt"

    def run_cli(self, *options):
        output, errors = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(sys, "argv", [str(SCRIPT), "--file", str(self.path), *options]),
            contextlib.redirect_stdout(output),
            contextlib.redirect_stderr(errors),
        ):
            try:
                self.builder.main()
            except SystemExit as error:
                return error.code, output.getvalue(), errors.getvalue()
        return 0, output.getvalue(), errors.getvalue()

    def test_bom_is_removed_before_first_value_and_deduplication(self):
        self.path.write_bytes(b"\xef\xbb\xbfRevenue\nCost\nRevenue\n")
        original = self.path.read_bytes()
        code, output, errors = self.run_cli()
        self.assertEqual(code, 0, errors)
        root = ET.fromstring(output)
        self.assertEqual(root.get("uniqueCount"), "2")
        self.assertEqual(
            [node.text for node in root.iter(f"{{{self.builder.SST_NS}}}t")], ["Revenue", "Cost"]
        )
        self.assertEqual(self.path.read_bytes(), original)

    def test_index_output_uses_same_bom_free_values(self):
        self.path.write_text("Revenue\nCost\n", encoding="utf-8-sig")
        code, output, errors = self.run_cli("--index")
        self.assertEqual(code, 0, errors)
        self.assertIn("'Revenue'", output)
        self.assertNotIn("\\ufeff", output)

    def test_embedded_bom_is_preserved_as_value_content(self):
        self.path.write_text("Revenue\n\ufeffCost\n", encoding="utf-8-sig")
        self.assertEqual(self.builder.load_from_file(str(self.path)), ["Revenue", "\ufeffCost"])

    def test_plain_utf8_and_whitespace_content_keep_existing_behavior(self):
        self.path.write_text("Revenue\n  Cost  \n\n", encoding="utf-8")
        self.assertEqual(self.builder.load_from_file(str(self.path)), ["Revenue", "  Cost  "])

    def test_invalid_utf8_is_clean_error_with_no_partial_xml(self):
        self.path.write_bytes(b"Revenue\n\xffCost\n")
        original = self.path.read_bytes()
        code, output, errors = self.run_cli()
        self.assertEqual(code, 1)
        self.assertEqual(output, "")
        self.assertIn("UTF-8", errors)
        self.assertNotIn("Traceback", errors)
        self.assertEqual(self.path.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
