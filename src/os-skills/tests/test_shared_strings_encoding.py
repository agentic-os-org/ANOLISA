"""Check SpreadsheetML escaping using real XML parsing and CLI output."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "others/xlsx/scripts/shared_strings_builder.py"
)
SPEC = importlib.util.spec_from_file_location("shared_strings_builder", SCRIPT)
builder = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(builder)
NS = {"s": builder.SST_NS}


class SharedStringEncodingTests(unittest.TestCase):
    def text(self, source: str) -> str:
        root = ET.fromstring(builder.build_xml([source]))
        return root.find("s:si/s:t", NS).text or ""

    def test_xml_invalid_controls_are_encoded(self) -> None:
        for character, expected in [
            ("\0", "_x0000_"),
            ("\x01", "_x0001_"),
            ("\x08", "_x0008_"),
            ("\x0b", "_x000B_"),
            ("\x0c", "_x000C_"),
            ("\x1f", "_x001F_"),
        ]:
            with self.subTest(character=repr(character)):
                self.assertEqual(self.text("a" + character + "b"), "a" + expected + "b")

    def test_carriage_returns_do_not_normalize_to_line_feeds(self) -> None:
        self.assertEqual(self.text("before\rafter"), "before_x000D_after")
        self.assertEqual(self.text("before\r\nafter"), "before_x000D_\nafter")

    def test_literal_escape_sequences_are_protected(self) -> None:
        for source, expected in [
            ("_x0001_", "_x005F_x0001_"),
            ("_x000D_", "_x005F_x000D_"),
            ("_x005f_", "_x005F_x005f_"),
        ]:
            with self.subTest(source=source):
                self.assertEqual(self.text(source), expected)

    def test_invalid_bmp_characters_produce_well_formed_xml(self) -> None:
        for source, expected in [
            ("\ufffe", "_xFFFE_"),
            ("\uffff", "_xFFFF_"),
            ("\ud800", "_xD800_"),
            ("\udfff", "_xDFFF_"),
        ]:
            with self.subTest(source=repr(source)):
                self.assertEqual(self.text(source), expected)

    def test_plain_unicode_whitespace_and_xml_characters_are_preserved(self) -> None:
        source = " 财务 & <data> 😀\t\n "
        self.assertEqual(self.text(source), source)
        node = ET.fromstring(builder.build_xml([source])).find("s:si/s:t", NS)
        self.assertEqual(
            node.get("{http://www.w3.org/XML/1998/namespace}space"), "preserve"
        )

    def test_non_escape_patterns_stay_literal(self) -> None:
        source = "_X0041_ _x123_ _xGGGG_ ordinary_underscore"
        self.assertEqual(self.text(source), source)

    def test_original_values_remain_distinct_during_deduplication(self) -> None:
        unique = builder.deduplicate(["\x01", "_x0001_", "\x01"])
        self.assertEqual(unique, ["\x01", "_x0001_"])
        root = ET.fromstring(builder.build_xml(unique))
        self.assertEqual(
            [node.text for node in root.findall("s:si/s:t", NS)],
            ["_x0001_", "_x005F_x0001_"],
        )

    def test_file_input_cli_emits_parseable_xml_and_original_indices(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "strings.txt"
            source.write_text("a\x01b\n_x0001_\na\x01b\n", encoding="utf-8")
            result = subprocess.run(
                [sys.executable, "-X", "utf8", str(SCRIPT), "--file", str(source)],
                capture_output=True,
                text=True,
                encoding="utf-8",
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        root = ET.fromstring(result.stdout)
        self.assertEqual(root.get("uniqueCount"), "2")
        self.assertEqual(
            [node.text for node in root.findall("s:si/s:t", NS)],
            ["a_x0001_b", "_x005F_x0001_"],
        )
        self.assertIn("1 duplicate(s) removed", result.stderr)

    def test_index_mode_still_displays_original_values(self) -> None:
        result = subprocess.run(
            [sys.executable, "-X", "utf8", str(SCRIPT), "--index", "_x000D_", "plain"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("0       '_x000D_'", result.stdout)
        self.assertIn("2 unique strings", result.stdout)


if __name__ == "__main__":
    unittest.main()
