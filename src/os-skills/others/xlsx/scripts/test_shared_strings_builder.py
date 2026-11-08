#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for shared_strings_builder.py ST_Xstring encoding.

Regression tests for issue #9692: the builder used to apply only HTML
escaping, which produced XML that parsers reject for control characters
(e.g. "\x01"), silently turned carriage returns into line feeds during
XML line-end normalization, and left literal "_xHHHH_" sequences exposed
to SpreadsheetML escape decoding.
"""

import importlib.util
import os
import re
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "shared_strings_builder.py")

sys.path.insert(0, SCRIPTS_DIR)

import shared_strings_builder as builder  # noqa: E402

NS_MAIN = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"


def cell_texts(xml: str) -> list:
    """Parse sharedStrings.xml and return each <t> text."""
    root = ET.fromstring(xml)
    return [si.find(f"{NS_MAIN}t").text for si in root.findall(f"{NS_MAIN}si")]


class StXstringEscapeTest(unittest.TestCase):
    def test_literal_escape_token_is_protected(self):
        """A literal "_xHHHH_" must be escaped so Excel's decoder cannot
        mistake it for an encoded character."""
        self.assertEqual(builder.st_xstring_escape("_x0041_"), "_x005F_x0041_")
        self.assertEqual(
            builder.st_xstring_escape("a_x0041_b"), "a_x005F_x0041_b"
        )
        # Already-escaped underscores are escaped again (idempotence is NOT
        # a goal; Excel behaves the same way).
        self.assertEqual(
            builder.st_xstring_escape("_x005F_x0041_"),
            "_x005F_x005F_x0041_",
        )

    def test_non_escape_underscore_runs_pass_through(self):
        """Sequences that merely look almost like escapes stay untouched."""
        self.assertEqual(builder.st_xstring_escape("_x0_"), "_x0_")
        self.assertEqual(builder.st_xstring_escape("_xA01z"), "_xA01z")
        self.assertEqual(builder.st_xstring_escape("_x00"), "_x00")
        self.assertEqual(builder.st_xstring_escape("under_score"), "under_score")

    def test_control_characters_are_encoded(self):
        """Characters illegal in XML 1.0 get the _xHHHH_ escape with
        uppercase hex; \t and \n stay literal."""
        self.assertEqual(builder.st_xstring_escape("a\x00b"), "a_x0000_b")
        self.assertEqual(builder.st_xstring_escape("a\x01b"), "a_x0001_b")
        self.assertEqual(builder.st_xstring_escape("a\x08b"), "a_x0008_b")
        self.assertEqual(builder.st_xstring_escape("a\x0bb"), "a_x000B_b")
        self.assertEqual(builder.st_xstring_escape("a\x1fb"), "a_x001F_b")
        self.assertEqual(builder.st_xstring_escape("tab\there"), "tab\there")
        self.assertEqual(
            builder.st_xstring_escape("line\nfeed"), "line\nfeed"
        )

    def test_carriage_return_is_preserved(self):
        """CR must survive both the builder and XML line-end normalization."""
        self.assertEqual(
            builder.st_xstring_escape("before\rafter"), "before_x000D_after"
        )
        xml = builder.build_xml(["before\rafter"])
        self.assertEqual(cell_texts(xml), ["before_x000D_after"])

    def test_noncharacters_are_encoded(self):
        self.assertEqual(builder.st_xstring_escape("￾"), "_xFFFE_")
        self.assertEqual(builder.st_xstring_escape("￿"), "_xFFFF_")

    def test_ordinary_unicode_and_xml_meta_pass_through_the_st_layer(self):
        self.assertEqual(
            builder.st_xstring_escape("中文 & <tags>"), "中文 & <tags>"
        )


class BuildXmlTest(unittest.TestCase):
    def test_control_character_produces_parseable_xml(self):
        """The exact failure from the issue: build_xml(['a\x01b']) used to
        emit a raw \x01 that ElementTree rejects."""
        xml = builder.build_xml(["a\x01b"])
        ET.fromstring(xml)  # must not raise
        self.assertEqual(cell_texts(xml), ["a_x0001_b"])

    def test_literal_escape_token_survives_roundtrip(self):
        xml = builder.build_xml(["a_x0041_b"])
        ET.fromstring(xml)
        self.assertEqual(cell_texts(xml), ["a_x005F_x0041_b"])

    def test_xml_meta_characters_still_escaped(self):
        xml = builder.build_xml(["R&D", "a<b>c"])
        ET.fromstring(xml)
        self.assertEqual(cell_texts(xml), ["R&D", "a<b>c"])
        self.assertIn("R&amp;D", xml)
        self.assertIn("a&lt;b&gt;c", xml)

    def test_whitespace_preserve_attribute_behavior_retained(self):
        xml = builder.build_xml(["  padded  ", "plain"])
        ET.fromstring(xml)
        self.assertIn('xml:space="preserve"', xml)
        root = ET.fromstring(xml)
        si_elements = root.findall(f"{NS_MAIN}si")
        self.assertEqual(
            si_elements[0].find(f"{NS_MAIN}t").get(
                "{http://www.w3.org/XML/1998/namespace}space"
            ),
            "preserve",
        )
        self.assertIsNone(
            si_elements[1].find(f"{NS_MAIN}t").get(
                "{http://www.w3.org/XML/1998/namespace}space"
            )
        )

    def test_index_comments_and_header_retained(self):
        xml = builder.build_xml(["A", "B"])
        self.assertTrue(xml.startswith(builder.HEADER + "\n"))
        self.assertIn('count="2"', xml)
        self.assertIn('uniqueCount="2"', xml)
        self.assertIn("<!-- index 0 -->", xml)
        self.assertIn("<!-- index 1 -->", xml)

    def test_every_control_character_roundtrips(self):
        chars = [chr(c) for c in range(0x20) if c not in (0x09, 0x0A, 0x0D)]
        chars.append("\r")
        xml = builder.build_xml(["x" + c + "y" for c in chars])
        ET.fromstring(xml)
        for original, encoded in zip(chars, cell_texts(xml)):
            expected = "x" + builder.st_xstring_escape(original) + "y"
            self.assertEqual(encoded, expected)
            self.assertNotIn(original, encoded)  # raw control char gone


class DeduplicationTest(unittest.TestCase):
    def test_dedup_uses_original_strings(self):
        """A literal escape token and the control character it encodes are
        different original strings and must both be kept."""
        result = builder.deduplicate(["a\x01b", "_x0001_", "a\x01b", "_x0041_", "A"])
        self.assertEqual(result, ["a\x01b", "_x0001_", "_x0041_", "A"])

    def test_dedup_preserves_first_occurrence_order(self):
        self.assertEqual(
            builder.deduplicate(["b", "a", "b", "c", "a"]), ["b", "a", "c"]
        )


class CliTest(unittest.TestCase):
    def run_cli(self, *args, stdin=None):
        return subprocess.run(
            [sys.executable, SCRIPT, *args],
            capture_output=True,
            text=True,
            input=stdin,
        )

    def test_cli_argument_input_encodes_and_parses(self):
        result = self.run_cli("a\x01b", "before\rafter", "a_x0041_b")
        self.assertEqual(result.returncode, 0, result.stderr)
        ET.fromstring(result.stdout)
        self.assertEqual(
            cell_texts(result.stdout),
            ["a_x0001_b", "before_x000D_after", "a_x005F_x0041_b"],
        )

    def test_cli_file_input(self):
        with tempfile.TemporaryDirectory() as root:
            path = os.path.join(root, "strings.txt")
            with open(path, "w", encoding="utf-8") as f:
                f.write("a\x01b\nplain\n")
            result = self.run_cli("--file", path)
            self.assertEqual(result.returncode, 0, result.stderr)
            ET.fromstring(result.stdout)
            self.assertEqual(cell_texts(result.stdout), ["a_x0001_b", "plain"])

    def test_cli_dedup_note_goes_to_stderr(self):
        result = self.run_cli("dup", "dup")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(cell_texts(result.stdout), ["dup"])
        self.assertIn("duplicate", result.stderr)

    def test_cli_index_table_still_lists_original_strings(self):
        result = self.run_cli("--index", "a\x01b")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("'a\\x01b'", result.stdout)


class XlsxWriterCompatibilityTest(unittest.TestCase):
    """Byte-for-byte agreement with XlsxWriter's ST_Xstring encoding, when
    the optional reference writer happens to be installed. Skipped
    otherwise — the required assertions above pin the exact escape text."""

    @unittest.skipUnless(
        importlib.util.find_spec("xlsxwriter"),
        "xlsxwriter not installed",
    )
    def test_text_bodies_match_xlsxwriter(self):
        import zipfile

        import xlsxwriter

        cases = [
            "a_x0041_b",
            "x\x01y",
            "before\rafter",
            "_x005F_ literal",
            "tab\there\nnl",
            "amp&<lt>",
            "￾￿",
            "  padded  ",
            "中文",
            "_x0",
            "_xA01z",
            "mixed_x0041_and\x02tail",
        ]
        with tempfile.TemporaryDirectory() as root:
            workbook = xlsxwriter.Workbook(os.path.join(root, "ref.xlsx"))
            sheet = workbook.add_worksheet()
            for index, text in enumerate(cases):
                sheet.write(index, 0, text)
            workbook.close()
            with zipfile.ZipFile(os.path.join(root, "ref.xlsx")) as z:
                reference = z.read("xl/sharedStrings.xml").decode("utf-8")

        mine = builder.build_xml(cases)
        pattern = re.compile(r"<t(?: [^>]*)?>(.*?)</t>", re.S)
        self.assertEqual(
            pattern.findall(reference), pattern.findall(mine)
        )


if __name__ == "__main__":
    unittest.main()
