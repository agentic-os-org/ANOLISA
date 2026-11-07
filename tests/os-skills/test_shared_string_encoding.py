"""Validate ST_Xstring output from the standalone shared-string generator."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/others/xlsx/scripts/shared_strings_builder.py"
)
SPEC = importlib.util.spec_from_file_location("shared_string_test_builder", SCRIPT)
BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER)
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


class SharedStringEncodingTests(unittest.TestCase):
    def values(self, strings):
        root = ET.fromstring(BUILDER.build_xml(strings))
        return [node.text or "" for node in root.findall(f"{{{NS}}}si/{{{NS}}}t")]

    def test_xml_disallowed_characters_are_encoded_as_xstrings(self):
        values = [
            "a\x00b",
            "a\x01b",
            "a\x08b",
            "a\x0bb",
            "a\x1fb",
            "a\ufffeb",
            "a\uffffb",
            "a\ud800b",
            "a\udfffb",
        ]
        self.assertEqual(
            self.values(values),
            [
                "a_x0000_b",
                "a_x0001_b",
                "a_x0008_b",
                "a_x000B_b",
                "a_x001F_b",
                "a_xFFFE_b",
                "a_xFFFF_b",
                "a_xD800_b",
                "a_xDFFF_b",
            ],
        )

    def test_carriage_returns_are_preserved_without_xml_normalization(self):
        self.assertEqual(self.values(["first\r\nsecond\rthird"]), ["first\r\nsecond\rthird"])

    def test_literal_escape_sequences_escape_only_the_leading_underscore(self):
        self.assertEqual(
            self.values(
                ["Report_x000A_", "_x005F_", "_x00af_", "_x000A__x000D_", "normal_under_score"]
            ),
            [
                "Report_x005F_x000A_",
                "_x005F_x005F_",
                "_x005F_x00af_",
                "_x005F_x000A__x005F_x000D_",
                "normal_under_score",
            ],
        )

    def test_normal_unicode_xml_escaping_and_whitespace_are_unchanged(self):
        values = [" 财务 & <notes> 😀 ", "line\nnext\tvalue", '"quoted"']
        xml = BUILDER.build_xml(values)
        self.assertEqual(self.values(values), values)
        self.assertIn("&amp;", xml)
        self.assertIn("&lt;notes&gt;", xml)
        root = ET.fromstring(xml)
        first = root.find(f"{{{NS}}}si/{{{NS}}}t")
        self.assertEqual(first.get("{http://www.w3.org/XML/1998/namespace}space"), "preserve")

    def test_cli_file_input_deduplicates_original_strings_before_encoding(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "strings.txt"
            original = b"a\x01b\nReport_x000A_\na\x01b\n"
            path.write_bytes(original)
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--file", str(path)],
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=20,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            root = ET.fromstring(result.stdout)
            self.assertEqual(root.get("uniqueCount"), "2")
            self.assertEqual(root.get("count"), "2")
            self.assertEqual(
                [node.text for node in root.findall(f"{{{NS}}}si/{{{NS}}}t")],
                ["a_x0001_b", "Report_x005F_x000A_"],
            )
            self.assertIn("1 duplicate(s) removed", result.stderr)
            self.assertEqual(path.read_bytes(), original)

    def test_index_output_keeps_original_values(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--index", "Report_x000A_", "Report_x000A_"],
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("'Report_x000A_'", result.stdout)
        self.assertNotIn("_x005F_", result.stdout)
        self.assertIn("Total: 1 unique strings", result.stdout)


if __name__ == "__main__":
    unittest.main()
